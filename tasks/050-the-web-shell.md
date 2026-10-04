---
id: "050"
title: The web shell
milestone: v0.5
status: ready
depends_on: ["049"]
adrs: ["0034", "0030", "0029", "0033", "0035", "0037", "0024"]
size: L
---

# The web shell

## Goal

Make the browser a place a person can actually use Rimaia. `rimaia-server` serves the built
bundle at `/`, with the API at `/api/v1` on the same origin, and allows cross-origin calls
only from the Tauri origins, only with a bearer token (ADR-0034 point 6). The React app
gains the four things only a signed-in, multi-team, machine-less client needs:

- **a sign-in screen**, shown whenever there is no session;
- **an account page**: sessions, tokens, and the user's runners, each revocable
  (ADR-0030 points 2, 3 and 5);
- **a team switcher**, because a person in two teams sees two boards (ADR-0029
  Consequences), and 039 refuses an entity-less call that could mean either;
- **browser states for every local-only action** (ADR-0034 point 5), on every view that
  has one, 017's review view included: the name of the machine that holds the worktree
  instead of Open worktree, each of the user's runners shown with the board's facts about
  it and where its settings are changed (point 5 as its 2026-10-04 amendment narrows it),
  and a place for each runner's doctor result, which 054 reports and 069 fills in.

Solo mode does not change. It has no sign-in, no account page and no switcher (ADR-0030
point 7, ADR-0029 point 2), and existing tests change only where the acceptance criteria
say.

## Why now

049 gave `commands.ts` and `events.ts` their HTTP and SSE transports and the browser its
fixed `get_client_capabilities` answer. The browser can therefore reach the server, but
what it reaches is a desktop app with the desktop removed:

- `App.tsx` opens by calling `get_app_info`, a local command, and `DoctorBanner` runs
  `run_doctor` on every view. In a browser both are refused.
- `OpenInMenu.tsx`, `WorktreeSection.tsx`, half of Settings and the Runs view's queue
  controls call local commands and render a refusal where an answer should be.
- 017's review view, the screen ADR-0033 point 7 exists to make work on every client,
  opens the Open in… menu on `w` and calls `@tauri-apps/plugin-opener` on `o`. The second
  never reaches the transport at all: `openUrl` calls `invoke("plugin:opener|open_url")`
  directly, and in a browser there is no Tauri to answer it. 017 says 049 switches
  `src/lib/open.ts` to `window.open`, and 049 does not, so it is this task's.
- There is nowhere to sign in, and nothing to do with `unauthenticated` but show it.
- For a user in two teams, `list_tasks` is refused `invalid`, naming both teams (039).
  The board is empty with an error on it.

Every later team-mode UI task builds on this one. 051 adds team creation and invitations
to the switcher and the account page, 059 reuses the account page and switcher in the
connected desktop, and 061 extends the runner list with assignment and consent. They need
the shell to exist, and to be shaped around capabilities rather than around a platform
check, before they add to it.

## Scope

**1. The server serves the bundle** (`crates/server`, ADR-0034 point 6, D34's `fs`
feature).

- **Where the bundle is.** One setting on the server's configuration, read the way 046
  reads the others: `RIMAIA_WEB_ROOT`, an absolute path to the directory `npm run build`
  writes (`dist/`). When it is unset the server serves the API only, and `/` is `404`.
  When it is set to a path that is not a directory containing `index.html`, the server
  refuses to start, with a message naming the variable and the path (D11's "fails
  loudly"). A server that starts and serves a blank page is the failure this prevents.
- **What is served.** `tower_http::services::ServeDir` over the web root, mounted *after*
  the `/api/v1` router, which keeps 046's JSON fallback: no path under `/api/` ever reaches
  the bundle. `GET /` is `index.html`.
- **No fallback to `index.html` for other paths.** The app has route state, not URLs
  (`App.tsx`'s own comment), so an unknown path is a `404`, not the app. 047's sign-in
  callback redirects to `/`. A deep link is a later task's decision, and 051's invitation
  link is the first that may want one.
- **Caching, so an upgrade reload works.** `index.html` is sent with
  `Cache-Control: no-cache`. Files under `assets/`, whose names Vite hashes, are sent
  with `Cache-Control: public, max-age=31536000, immutable`. Without the first, the reload
  in Scope 6 would fetch the stale bundle it is trying to replace.
- **Two headers on every bundle response:** `X-Content-Type-Options: nosniff` and
  `Content-Security-Policy: frame-ancestors 'none'`. The account page has revoke buttons,
  and a page that can be framed can have them clicked for it. They are set with axum's own
  middleware (`axum::middleware::map_response`), because D34 approves `tower-http`'s
  `trace`, `cors` and `fs` features and not `set-header`. A full script and style CSP is
  out of scope.

**2. CORS for the Tauri origins, and only with a bearer token** (D34's `cors` feature).

- **The allow-list is a pure function in `crates/server`, `allowed_origins(dev: bool)`.**
  It returns exactly `tauri://localhost`, `http://tauri.localhost` and
  `https://tauri.localhost`: the origins Tauri 2's webview uses on macOS and Linux, and on
  Windows with and without `useHttpsScheme`. With `dev = true` it adds
  `http://localhost:1420`, the `devUrl` in `src-tauri/tauri.conf.json`, so a connected
  `npm run tauri dev` (059) can reach a local server. **The router builder takes `dev` as
  an argument.** The binary passes `cfg!(debug_assertions)`, so a release build never
  allows the dev origin, and the tests pass `false` or `true` themselves, because
  `cfg!(debug_assertions)` is always true under `cargo test`.
- **The layer:** `CorsLayer` with those origins listed exactly (never a predicate and
  never `Any`), methods `GET`, `POST` and `OPTIONS`, request headers `authorization`,
  `content-type`, `rimaia-protocol` and `rimaia-team` (Scope 3), and a preflight
  `max_age` of ten minutes. **`allow_credentials` is never set.** A browser therefore never
  sends the `rimaia_session` cookie cross-origin, so a Tauri origin can authenticate only
  with a bearer token. The cookie and the `X-Rimaia-CSRF` header stay same-origin only.
  `x-rimaia-csrf` is deliberately not an allowed header.
- It applies to `/api/v1/*`, including `/api/v1/events`. It does not apply to the bundle,
  and it does not apply to `/api/v1/runner/*`, whose callers are not browsers.

**3. The team switcher's wire: a `Rimaia-Team` request header.** No ADR or seam entry makes
this decision (046's Out of scope leaves it here), so this task writes it into the seam
contract (Scope 8).

- **What it means.** On the two surfaces in D32 point 7's first row, `POST
  /api/v1/<board command>` and `GET /api/v1/events`, a `Rimaia-Team: <team id>` header
  narrows the request's `Caller` to that one team. The narrowing happens once, at the edge,
  in core: `Caller::narrow_to(&self, team_id: &str) -> Result<Caller>` in
  `crates/core/src/api/caller.rs`. It returns the caller with `teams` reduced to that one
  grant, and `Error::not_found` when the caller has no grant for it, with the same message
  a missing team gets. The server's `Caller` extractor calls it after `Authenticate`. The
  extractor holds no rule of its own (ADR-0006).
- **Commands about the person, not the team, are never narrowed.**
  `api::registry::ignores_team_header(name) -> bool` is true for exactly these rows, and
  the extractor asks it before calling `narrow_to`: `list_teams`, `list_runners` and
  `unpair_runner` below, 047's eight account rows, and `get_subscription_cost` and
  `set_subscription_cost` (a user setting). Their `Caller.teams` is every membership, as
  `Authenticate` read it, so `get_account` lists every team and
  `create_personal_access_token` accepts a team other than the one on screen. 051 adds its
  rows to the same function.
- **The events stream is narrowed too, and stays narrowed.** 048's revalidation closure
  re-runs the route's whole caller resolution, `narrow_to` included, with the header the
  stream opened with (048 Scope 7). A narrowed stream therefore resolves to the same one
  team on every revalidation and is not ended, and a person who loses that team has the
  stream ended and its reconnect refused `not_found`.
- **Nothing else reads it either.** `/mcp`: ADR-0035 point 2 rejects an implicit current
  team for agents, and 060 gives tools a `team` argument instead. The runner protocol: a
  lease determines the team (ADR-0029 point 5). The solo shell's `invoke` has no headers,
  and its caller already has exactly one team.
- **Why a header and not an argument.** Every entity-less board command would otherwise
  need a new `team` field. The invoke payloads that 24 frontend test files assert on
  would change, and D32 point 1 requires the wire shapes to be identical on both
  transports. A header is transport metadata, as `Rimaia-Protocol` is. It is sent on
  every request and stored nowhere on the server, so it is not the "current team stored per
  token" that ADR-0035 rejects. The person sees which team they are in on every screen.

**4. Three board commands.** 047 adds none of them, and 051, 059, 060, 061 and 063 name
them. Each is a board row in `api::registry` with a core handler under
`crates/core/src/api/board/`, a `board<T>` wrapper in `commands.ts`, a fixture row (028),
and a `BoardCase` in 046's `BOARD_CASES` (`crates/core/src/testing/api.rs`).

| Command | Effect | Answers |
| --- | --- | --- |
| `list_teams` | Read | The caller's memberships, read by `user_id` from `team_memberships` joined to `teams`: `{ id, name, role, personal }`, where `personal` is `teams.personal_user_id = caller.user_id`. Personal team first, then by name |
| `list_runners` | Read | The caller's own runners (`runners.user_id = caller.user_id`, `unpaired_at IS NULL`): `{ id, label, provider, appVersion, eligibility, pairedAt, lastSeenAt }`. Never another user's runner, even one in a shared team: a runner works for exactly one user (ADR-0030 point 5) |
| `unpair_runner` | Write | `{ id }`. Checks that the runner is the caller's and not unpaired, then calls 047's `identity::tokens::unpair_runner`, the function `revoke_api_token` already uses, and writes nothing of its own. Another user's runner, or one already unpaired, is `not_found` |

`get_account`'s team list does not replace `list_teams`: it has no `personal`, which the
switcher's fallback needs, and 060 pairs its `list_teams` tool with this row (ADR-0035
point 2). 057 adds releasing pins to 047's function, so it reaches this command with no
change here. No new `ChangeEvent`: the account page re-reads after its own write, and D8
and ADR-0018's event list do not grow for a page that one person looks at.

**5. The machine that holds the worktree** (ADR-0034 point 5, ADR-0033 point 3).

- `TaskDetail` gains `worktreeRunner: { id, label, unpaired } | null`, mirrored in
  `src/types.ts`. A new small query beside `fetch_last_run` in
  `crates/core/src/tasks/service.rs` reads it: the `runner_id` of the row `fetch_last_run`
  picks, joined to `runners`. `Run` does not widen, because `fetch_last_run`'s
  `query_as!(Run, …)` shares that struct with every other run reader. Like that reader,
  the field takes every kind (D29 point 4): implementation, review and fix runs all work in
  the task's one worktree (ADR-0005). A task with no run is `null`. MCP's `TaskView`
  (`crates/core/src/mcp/responses.rs`) declines the field in its exhaustive
  destructuring, with a comment: the solo endpoint has one machine, and whether hosted
  agents see machine names is 060's parity decision.
- **The rule, in one place:** `src/lib/worktreeHere.ts`,
  `worktreeIsHere(task, capabilities): boolean`. It is true in solo mode. Otherwise it is
  true only when the capabilities name a local runner and that runner is
  `task.worktreeRunner.id`. `WorktreeSection.tsx` and `ReviewView.tsx` call it. When it is
  false `WorktreeSection` renders "Worktree on {label}" (with "(unpaired)" when the runner
  is unpaired), and calls no local command. That covers the browser and the connected
  desktop whose runner is not the one that ran the task.
- **Outside solo, the card has no Open in….** `TaskCard.tsx` renders from `TaskSummary`,
  which names no runner, and D12's bulk read does not change here. So the card renders
  `OpenInMenu` in solo only, and the machine's name appears only in the panel and the
  review view. This is a
  known limitation for 059: a connected desktop reaches its own worktree from the panel,
  not the card. Putting the runner on `TaskSummary` is a D12 amendment, 059's to make if it
  wants the card's menu back.
- **The review view asks the same rule.** 017's `ReviewView` already reads `get_task` for
  the task on screen, so it has `worktreeRunner` with no new read. When
  `worktreeIsHere(task, capabilities)` is true, `w` opens `OpenInMenu` exactly as 017 built
  it. When it is false, `w` is inert, the legend drops its `w` row, and the task shows the
  same "Worktree on {label}" line `WorktreeSection` renders, through one shared component,
  `src/components/WorktreeElsewhere.tsx`, so the two places cannot word it differently.
  With `worktreeRunner` `null` outside solo, nothing is shown and `w` is inert. Neither
  case renders `OpenInMenu` or sends `list_open_in_targets`.

**6. The shell in the browser.** Every choice below comes from 049's
`get_client_capabilities` answer (its mode, local runner and local actions). Nothing
checks for a platform: no `isTauri`, no `window.__TAURI__`, no user-agent sniffing
(ADR-0034 point 5).

- **How components get the answer.** `bootstrapClient()` resolves with the answer it
  installed transports for, and `main.tsx` passes it to a `ClientCapabilitiesProvider` in
  `src/lib/capabilities.tsx`. Components read it through `useClientCapabilities()`, the
  hook 049 left to this task, and never call `getClientCapabilities()`. With no provider
  mounted the hook returns the solo answer. Existing tests mount none, so they render solo
  and send no `get_client_capabilities` to `invoke` mocks that reject unknown commands. The
  fixture entry mounts the provider with its scenario's answer (Scope 7).
- **Sign-in.** When the mode is not solo, `App.tsx` first calls `get_account`.
  `unauthenticated` renders `src/views/SignInView.tsx`: the product name, one sentence,
  and in browser mode a "Sign in with GitHub" link to `/auth/github`. It is a plain link,
  not a `fetch`, because OAuth needs a top-level navigation. Connected mode cannot be
  entered before 059 (049 refuses it), and 059 supplies its loopback sign-in action in that
  place (ADR-0030 point 4), so the browser-cookie link is never rendered in a desktop. From
  then on, any `unauthenticated` from any command or from
  `subscribeToEventStreamFailure` returns to that screen. 049's transport already surfaces
  the code, and this task adds the one place that reacts to it, in `src/lib/`, so no
  component handles it.
  - **Why sign-in failed.** 047 redirects a failed callback to `/?sign_in=denied`,
    `expired` or `failed`. `SignInView` reads `sign_in` from `location.search`, removes it
    with `history.replaceState`, and renders one sentence above the link:
    `denied`, "GitHub did not grant access, so you are not signed in."; `expired`, "That
    sign-in took too long and has expired. Sign in again."; `failed`, "Signing in did not
    work. Try again in a minute." Any other value renders the plain screen and is never
    echoed.
- **`upgrade_required` reloads the page once per bundle** (D32 point 7, "050"). This
  applies only in browser mode. The HTTP transport compares `sessionStorage`'s
  `rimaia.upgradeReload` with `PROTOCOL_VERSION`, the literal in `commands.ts`. When
  nothing is stored or the values differ, it stores `PROTOCOL_VERSION` and calls
  `location.reload()`. When they are equal, this bundle has already been reloaded into and
  is still too old, so it does not reload. The error then reaches the caller, and `App`
  shows one banner: "This page is older than the server, and reloading did not fix it. Try
  again in a minute." Nothing clears the value. Board Reads are answered under skew (D32
  point 7), so a success says nothing about the bundle, and a bundle that fixes the skew
  carries a different literal. In the desktop modes, `upgrade_required` is left to 063's
  updater and is not reloaded.
- **No local command on the way in.** In browser mode, `App.tsx` does not call
  `get_app_info`, never opens on the welcome screen (onboarding checks this machine), and
  does not render `DoctorBanner`. The sidebar shows no app version.
- **Opening a link follows the mode, not the platform.** 017's `src/lib/open.ts` gains a
  setter, `setExternalUrlOpener(opener)`, in the pattern of 049's transport setters, and
  `installTransports("browser")` calls it with an opener that calls
  `window.open(url, "_blank", "noopener,noreferrer")`. The default stays 017's `openUrl`
  from `@tauri-apps/plugin-opener`, so solo and connected are unchanged and 017's `o` test
  passes unedited. `openExternalUrl` calls the opener before any `await`, because a browser
  lets a page open a tab only during the keypress that asked for it. No component changes:
  `ReviewView`'s `o` still calls `openExternalUrl`, and only `open.ts` imports the plugin.
- **Only `https:` and `http:` links are opened, in every mode.** `pr_url` is text a run
  recorded from an agent's output. In the browser, `window.open("javascript:…")` would run
  that text as script on the server's origin, with the session cookie, one request from
  the account page's revoke buttons. `openExternalUrl` parses the URL with `new URL`, and
  for any other protocol, or a string that does not parse, opens nothing and rejects with
  the sentence "Only web links can be opened.", which the review view renders as 017
  renders any refusal. The value is never echoed. The desktop gets the same check, so a
  link the browser refuses is never one the desktop opens.
- **The team switcher**, `src/components/TeamSwitcher.tsx`, sits at the top of
  `Sidebar.tsx` in every mode but solo. It reads `list_teams`, shows the current team's
  name, and opens a menu only when there is more than one team.
  - **Choosing a team** sets the transport's team, stores its id in `localStorage` under
    `rimaia.team`, calls `reconnectEventStream()` so the stream carries the new header,
    and remounts the content area keyed by team id, so no open panel keeps an id from the
    team it left.
  - **A team that is gone** falls back to the personal team, or to the first team if there
    is none: at startup, when `list_teams` no longer returns the stored id, and when the
    stream is refused `not_found` and a fresh `list_teams` no longer returns it.
  - **No board read is sent before a team is chosen.** The only requests before then are
    `get_account` and `list_teams`.
  - The team is state in `commands.ts`, and `transportHeaders` adds the header, as 049
    asks. No component sets a header.
- **The account page**, `src/views/AccountView.tsx`, with its sections in
  `src/views/account/`, is a sidebar entry in every mode but solo. It is built on 047's
  eight account rows.
  - **You:** login and avatar from `get_account`, and **Sign out**: `revoke_session` with
    `get_account`'s `current.id` (047 Scope 12), then the sign-in screen at once, without
    waiting for the next request to be refused. 047 has no sign-out route, because
    `board<T>` is the only HTTP sender (D32 point 6), and the two cookies it leaves hold a
    dead secret.
  - **Sessions:** `list_sessions`, with device (the stored user agent, shortened), created
    and last used. The current session is marked, and every row has Revoke
    (`revoke_session`). Revoking the current session is Sign out.
  - **Tokens:** `list_api_tokens`, with kind, label, created, last used, where last used,
    and expiry, for each `rmd_`/`rmr_`/`rmp_` token, each with Revoke
    (`revoke_api_token`). "New personal access token" calls
    `create_personal_access_token` with a label, an optional subset of the user's teams
    (ADR-0030 point 6) and an optional expiry in days. **The secret is shown once**, with a
    copy button and the sentence "This is the only time it will be shown". It is dropped
    from component state once the dialog closes. It is never written to storage, never
    logged, and never part of a re-read.
  - **Runners:** `list_runners`, with label, provider, version and last seen, and Unpair
    behind a confirmation naming the runner. "Pair a runner" calls `create_pairing_code`
    and shows the code with its expiry and the exact line
    `rimaia-runner pair <origin> <code>`, where `<origin>` is `window.location.origin`.
- **Local-only controls when there is no local runner.** Each gate is a capability check
  with a one-line comment naming the command that forces it. Where the D32 appendix has a
  later task flip that command to `board`, that task deletes the gate in the same commit.

  | Commands | Called from | Gate deleted by |
  | --- | --- | --- |
  | `get_app_info` | `App.tsx` (welcome screen, version), `StorageSection` | stays local |
  | `run_doctor`, `dismiss_doctor_warning` | `useDoctor` (`DoctorBanner`, `DoctorSection`) | stays local |
  | `list_open_in_targets`, `open_task_worktree_in` | `OpenInMenu`, on the card and from `ReviewView`'s `w` (Scope 5) | stays local |
  | `plugin:opener\|open_url`, not a command | `src/lib/open.ts`, from `ReviewView`'s `o` | stays: the browser's opener replaces it (above), and no component gates it |
  | `get_worktree_status`, `reveal_task_worktree` | `WorktreeSection` (Scope 5) | stays local |
  | `get_diff_summary` | `RunReviewSections` with 017's `liveDiff: "fallback"`, from `RunDetailOverlay` | stays local. The overlay passes `"fallback"` in solo only and `"none"` in every other mode: the fallback reads a branch as it is now on the machine that holds it, and serves only runs recorded before 033, so a connected desktop loses it for those rows alone |
  | `cancel_task_run` | `ActiveRunCard` | 052 |
  | `start_task_run` / `retry_task_now` | `TaskCard` / `RetrySection` | 069, which adds the runner picker these need; 052 flips them and keeps the gate |
  | `get_queue_status` and the queue commands | `TaskCard`, `QueuePlanList`, `RunsView`, `QueueControls` | stays local |
  | `get_run_environment`, `set_run_environment`, `preview_composed_prompt` | `InstructionsSection`, `RunsView` | stays local |
  | `plan_task_strategy` / `plan_tasks_strategy`, `cancel_plan_pass` | `panel/StrategySection` / `board/PlanPassPanel` | stays local (D32's amendment on the planning commands); 061 gives the browser its own Plan buttons over 060's request commands |
  | `read_run_transcript_page`, `search_run_transcript` / `summarize_run_transcript` | `TranscriptViewer` / `RunDetailOverlay` | 056 |
  | `reveal_run_log` | `RunDetailOverlay` | stays local |
  | `register_repository` (Add repository) | `RepositoryAddForm` | stays local: 054 points the form at the local `add_repository_from_clone`, and 069 replaces this gate with a form over the board `register_repository` |
  | `set_repository_unattended_runs`, `set_repository_on_archive`, `set_repository_max_concurrency`, `get_repository_remote_info`, the three credential commands | `RepositoriesSection`, `CredentialSection` | stays local |

- **Settings in the browser.** Repositories, Instructions and Strategy stay, with the gates
  above. Concurrency, Schedules, MCP, Storage, Doctor and the debug-only Developer section
  exist only on a machine, and are replaced by one section,
  `src/views/settings/RunnersSection.tsx`. It is ADR-0034 point 5's read-only runner
  settings as that ADR's 2026-10-04 amendment narrows them: the board holds no runner
  setting (D28 part 4 puts every one in `runner.db`), so the section shows no setting's
  value and says where each is changed. For each of `list_runners`:
  - the board's view of it: label, provider, version, eligibility, and last seen
    (relative, from the injected `now` that `src/lib/format.ts`'s callers already use);
  - "{label}'s settings are changed on that machine, not here." The board cannot tell a
    desktop runner from a headless one (058), so the sentence names neither the desktop app
    nor a command line;
  - "{label} has not reported a doctor result yet." The board has no doctor result until
    054 adds `list_runner_doctor_reports`, a summary of check ids and statuses with no
    prose, and 069 renders it here in place of the sentence.

  The Analytics view needs no gate: its subscription cost is a board user setting.
- **Design.** ADR-0024 throughout: sentence case, no uppercase transforms, state as a dot
  and a word, one accent, fluid width, and light and dark designed equally. New styles go
  in the existing leaf stylesheets' idiom and use the existing tokens.

**7. Screenshots in both schemes** (task 028's mechanism, not a new one). The fixture
table gains a row for every command this task adds or starts calling, and its
`get_client_capabilities` answer becomes per scenario. New scenarios:

| Scenario | Seeds | Views |
| --- | --- | --- |
| `signed-out` | browser capabilities, `get_account` refused `unauthenticated` | sign-in |
| `browser` | browser capabilities, two teams (personal first), the `busy` board, a task whose `worktreeRunner` is another machine, three runners (one never seen), and that task in `in_review` with 017's recorded-bundle seed | board with the switcher open, board with that task's detail open, review queue on that task, settings (runners section), account |
| `browser-one-team` | as `browser` with one team | board |

That adds seven captures in each of 028's four projects. The review capture uses 017's
per-row key sequence to reach the queue, and never presses `o` or `w`.

**8. The seam contract.** This task appends the next free D entry, "Task 050's
cross-cutting choices", in the four-part shape, recording: the `Rimaia-Team` header,
`Caller::narrow_to`, `ignores_team_header` and its rows, and the stream narrowed through
048's revalidation closure (Scope 3); `RIMAIA_WEB_ROOT` and no `index.html` fallback; the
cache and framing headers; `allowed_origins` and the absence of `allow_credentials`; the
three commands and their user scoping; `TaskDetail.worktreeRunner`, `worktreeIsHere` as
the one rule the panel and the review view share, and the card's limitation;
`setExternalUrlOpener` and the `https:`/`http:` rule in `open.ts`; `RunnersSection`
showing no runner setting's value, pointing at ADR-0034's 2026-10-04 amendment rather than
restating it; and the one-reload rule keyed by `PROTOCOL_VERSION`. Its Binds line says that
051 adds its rows to `ignores_team_header`, and that 052 and 060 each carry a test that
their surface ignores `Rimaia-Team`. It also adds a row for 050 to "How to use this".
D32's Binds line for 050 already says "reloads the page on `upgrade_required`", and the
new entry points back to it.

**9. CLAUDE.md.** Beside 046's line on running the server, add how to run the web shell
locally: `npm run build`, then the server with `RIMAIA_WEB_ROOT` set to the absolute path
of `dist/`. `## Commands` does not change, because CI does not run this. **There is no Vite
dev proxy.** 047's callback returns to `RIMAIA_PUBLIC_URL`, so a sign-in started on Vite's
origin ends on the server's, and making that work through a proxy would rest on browsers
not separating cookies by port. Fixture mode (028) and vitest are the fast loop for these
screens.

## Out of scope

- **Teams as things a person manages.** Creating, renaming and deleting teams,
  invitations, roles and the last-owner rule are 051's. The switcher only switches.
- **Deleting an account.** That is 051's, with team deletion (ADR-0029 point 6).
- **A return path across sign-in.** 047's callback always lands on `/`. 051 keeps its
  invitation in `sessionStorage` across the round trip itself.
- **The `claude mcp add` line for a new personal token.** `/mcp` does not exist on the
  server until 060, and a line that connects to nothing is the bug `McpAddCommand.tsx`'s
  own comment describes. 060 adds it next to the token dialog.
- **Runner settings reported to the board.** ADR-0034's 2026-10-04 amendment narrows point
  5 to the board's own facts about each runner plus where its settings are changed, and
  says why: a reported copy is stale exactly when its runner is offline. A later task that
  wants a setting's value in the browser amends D28 and D31 first, and that amendment.
- **Each runner's doctor result.** 054 reports it and adds `list_runner_doctor_reports`;
  069 renders it in `RunnersSection`.
- **The browser form to register a repository by remote URL.** 054 adds the command and
  069 the form. Add repository stays hidden in the browser until then.
- **A review view designed for the browser.** The review view changes in two places, `w`
  and `o`, and nothing else. Its bundle sections are already board data (ADR-0033 point
  7), and 037's findings are too.
- **Showing which runners are out of date.** 063 does that (ADR-0037 point 5). This task
  shows the version and makes no judgement about it.
- **Assignment, consent, eligibility editing and pinned cards** in the interface. Those
  are 061's. `eligibility` is shown read-only.
- **Desktop sign-in and the connected mode chooser.** 059. The account page and switcher
  are built for any non-solo mode, so 059 inherits them, but 059 is what enters that mode
  on a desktop.
- **A router or URLs.** Route state stays as `App.tsx` has it.
- **A full content security policy.** `frame-ancestors` alone is in scope. A script and
  style policy needs checking against the bundle Vite emits, and 062 owns the production
  headers.
- **Mobile layouts.** ADR-0024's fluid layout covers the widths 028 captures. Phone widths
  are not a target.

## Acceptance criteria

Rust tests use the real SQLite harness and `crates/core`'s fake `Clock`, and contain no
`sleep`. The bundle tests write a real `index.html` and a hashed asset into a `TempDir`, and
never fake the filesystem. Frontend tests are vitest. They mock at `@tauri-apps/api/core`
and at 049's HTTP mock, never the wrappers (`StorageSection.test.tsx` explains why), and
assert exact command names, arguments and header values.

**The server, in `crates/server/tests/web.rs`:**

- `the_bundle_is_served_at_the_root`: `GET /` is `200`, `text/html`, the file's exact
  bytes, and `Cache-Control: no-cache`.
- `hashed_assets_are_cached_for_a_year`: `GET /assets/index-abc123.js` carries
  `public, max-age=31536000, immutable`.
- `an_api_path_never_falls_through_to_the_bundle`: `POST /api/v1/no_such_command` and
  `GET /api/v1/` are `404` with body `{"code":"not_found",…}` and are not `index.html`.
- `an_unknown_path_is_not_the_app`: `GET /account` is `404`.
- `a_path_outside_the_web_root_is_never_served`: `/../Cargo.toml` and its
  percent-encoded forms do not return a file from outside the root.
- `the_bundle_cannot_be_framed`: both Scope 1 headers are present on `/` and on an asset.
- `without_a_web_root_only_the_api_is_served` and
  `a_web_root_without_an_index_refuses_to_start`. The latter's error names
  `RIMAIA_WEB_ROOT` and the path.

**CORS, in `crates/server/tests/cors.rs`**, on routers built with an explicit `dev`:

- `allowed_origins_are_exactly_the_tauri_origins`: `allowed_origins(false)` equals the
  three origins, and `allowed_origins(true)` equals those plus `http://localhost:1420`.
- `a_tauri_origin_may_preflight_a_bearer_request`: for each of the three origins, an
  `OPTIONS` preflight for `POST /api/v1/list_tasks` echoes the origin and allows
  `authorization`, `rimaia-protocol` and `rimaia-team`, and it carries no
  `Access-Control-Allow-Credentials`.
- `any_other_origin_gets_no_cors_headers`: on a router built with `dev = false`,
  `https://evil.example`, `null` and `http://localhost:1420` get no
  `Access-Control-Allow-Origin`.
- `the_csrf_header_is_not_allowed_cross_origin`: a preflight asking for `x-rimaia-csrf`
  from a Tauri origin is not granted it.

**The team header**, as core unit tests and in `crates/server/tests/`:

- `narrowing_to_a_member_team_selects_it_and_keeps_every_grant` and
  `narrowing_to_a_foreign_team_is_answered_as_a_missing_one`: the second compares the
  error with a never-issued id's error for equality, in the way 039's registry test does.
- `a_two_team_caller_lists_one_board_with_the_team_header`: with a two-team session,
  `list_tasks` without the header is refused `invalid` naming both teams (039), and with
  `Rimaia-Team: <A>` it returns exactly team A's tasks.
- `person_scoped_commands_ignore_the_team_header`: `get_account`, `list_teams` and
  `list_runners` answer identically with `Rimaia-Team: <A>` and without it, and
  `create_personal_access_token { teamIds: [<B>] }` succeeds under both.
- `the_events_stream_carries_only_the_chosen_team`: on `/api/v1/events` with
  `Rimaia-Team: <A>`, a change published for team B is not delivered and one for team A is.
- `a_narrowed_stream_survives_revalidation`: the same stream is still open, and still
  team A's only, after the fake clock crosses 048's `STREAM_REVALIDATE_INTERVAL` with
  nothing changed. Once the caller's membership of A is deleted and the stream
  revalidated, it ends, and reopening it with the same header is `404 not_found`. Both
  read the stream with no `sleep`.

**The three commands**, as core tests and as `BoardCase`s:

- `list_teams_lists_every_membership_whatever_the_header_says`: a caller narrowed to A still
  lists A and B, personal first, with the exact roles.
- `list_runners_never_shows_another_users_runner`: two users in one team, with one
  runner each. Each sees only their own, and an unpaired runner is absent.
- `unpairing_a_runner_revokes_its_tokens_and_keeps_its_row`: afterwards the runner's
  `rmr_` token gets `unauthenticated` from `Authenticate`, `unpaired_at` equals the fake
  clock's instant exactly, and its runs still name it.
- `unpairing_another_users_runner_is_not_found`, answered exactly as a never-issued id.
- 046's `every_board_command_has_a_case`, `a_team_cannot_see_another_teams_ids` and
  `both_transports_answer_every_case_identically` pass with the new rows, and
  `./scripts/check-command-wiring.sh` passes.

**The worktree's machine:**

- `the_worktree_runner_is_the_newest_runs_runner_of_any_kind`: an implementation run on
  runner R1 followed by a review run on R2 gives R2, and a task with no run gives `null`.
- `an_unpaired_runner_is_still_named_as_the_worktrees_machine`.
- `src/lib/worktreeHere.test.ts`: true in solo; in browser mode false for any task; in
  connected mode true only when the local runner is the task's; false when
  `worktreeRunner` is `null` outside solo.
- `WorktreeSection` renders exactly "Worktree on Studio Mac" and "Worktree on Old laptop
  (unpaired)" when the rule is false, and sends neither `get_worktree_status` nor
  `reveal_task_worktree`.
- `it("renders no Open in… on a card outside solo")`: under browser and connected
  capabilities, a card with a worktree renders no `OpenInMenu` and sends no
  `list_open_in_targets`.
- In `ReviewView.test.tsx`, `it("names the worktree's machine instead of opening it")`:
  under browser capabilities, on a queued task whose `worktreeRunner` is "Studio Mac", the
  view renders exactly "Worktree on Studio Mac", the legend has no `w` row, and pressing
  `w` renders no `OpenInMenu` and sends no `list_open_in_targets`. Under connected
  capabilities whose local runner is the task's, `w` opens the menu as in solo. 017's
  existing `w` test passes unedited, because it mounts no provider and renders solo.
- `it("uses the live diff fallback in solo only")`: `RunDetailOverlay` on a
  `not_recorded` run sends `get_diff_summary` with no provider mounted, and sends nothing
  under browser or connected capabilities, rendering "No diff was recorded for this run."
  033's overlay tests pass unedited.

**The shell:**

- `it("shows sign-in when there is no session")` and
  `it("returns to sign-in when any later command is unauthenticated")`. The link's `href`
  is `/auth/github`, exactly, and the link is absent under connected capabilities.
- `it("explains a failed sign-in")`: for `denied`, `expired` and `failed`, the exact
  sentence renders and `history.replaceState` removed the parameter. `?sign_in=<b>x</b>`
  renders the plain screen, with no sentence and none of the value.
- `it("reloads once per bundle on upgrade_required")`: a write's `upgrade_required` stores
  `PROTOCOL_VERSION` and calls the mocked `location.reload` once. Then, as the reloaded
  page with the same literal, reads succeed, a write gets `upgrade_required` again,
  `location.reload` is not called a second time, and the banner's exact sentence renders.
  With a different value stored, `upgrade_required` reloads.
  `it("never reloads on upgrade_required in a desktop mode")`.
- `it("sends the chosen team on every board request and on the events stream")`: after
  choosing team B, the next `list_tasks` carries `Rimaia-Team: <B>`, and the events stream
  is reopened with the same header.
- `it("sends no board read before a team is chosen")`,
  `it("falls back to the personal team when the chosen team is gone")`, at startup and
  after the stream is refused `not_found`,
  `it("closes an open task panel when the team changes")`, and
  `it("shows the team name with no menu when there is one team")`.
- `it("renders no sign-in, account entry or switcher in solo mode")`.
- `it("shows a new token's secret once")`: after the dialog closes, the secret is in
  neither the DOM, `localStorage` nor `sessionStorage`, and a re-read of the token list
  does not bring it back.
- `it("signs out by revoking the current session")`, `it("revokes a token by its id")`,
  and `it("unpairs a runner only after the confirmation")`. Each asserts the exact command
  and arguments; the first also that the sign-in screen renders before any further
  request.
- **Opening a link, in `src/lib/open.test.ts`** (017's file, extended):
  `it("opens a link in a new tab in browser mode")`: after `installTransports("browser")`,
  `openExternalUrl("https://github.com/o/r/pull/7")` calls the mocked `window.open` once
  with exactly that URL, `"_blank"` and `"noopener,noreferrer"`, synchronously, and the
  `invoke` mock receives no `plugin:opener|open_url`.
  `it("opens only web links")`: `javascript:alert(1)`, `data:text/html,x`,
  `file:///etc/passwd` and `not a url` each reject with exactly "Only web links can be
  opened.", in browser and in solo, and neither `window.open` nor `invoke` is called.
  `http:` and `https:` open. In solo, 017's `plugin:opener|open_url` assertion is
  unchanged.
- `it("shows the pairing line with this origin")`, asserting the exact string
  `rimaia-runner pair https://rimaia.example ABCD-EFGH` for the fixture's origin and code.
- `RunnersSection`: `it("says where each runner's settings are changed")` and
  `it("says a runner has not reported a doctor result")`, with the exact sentences, and
  `it("shows no runner setting's value")`: the section sends no `get_run_environment`,
  `get_queue_status` or other local read, and renders no concurrency, schedule, run
  environment or MCP port. That is ADR-0034's amendment, held by a test.
- **No local command in the browser.**
  `it("sends no local command from any view in browser mode")` renders Board (with a task's
  detail open), Runs (with a `not_recorded` run's detail open), Review (on a queued task
  whose worktree is on another machine, with `w` and `o` pressed), Analytics, Settings
  and Account under browser capabilities. It asserts that no request other than
  `get_client_capabilities` reached the local transport, that the `invoke` mock received
  nothing (which catches `plugin:opener|open_url`, a call that bypasses the transport),
  that `window.open` was called once with the task's `pr_url`, and that the browser's
  "only available in the desktop app" refusal was never produced. A gate removed by
  mistake fails it.
- **Capabilities, not platforms.** `it("decides nothing from the platform")` scans
  `src/components`, `src/views`, `src/hooks` and `src/App.tsx` and finds no `isTauri`, no
  `__TAURI`, and no `navigator.userAgent`.
- **Existing frontend tests pass with two kinds of edit only**, which `git diff --stat`
  shows: fixture coverage (028) gains the new rows, and each `TaskDetail` builder
  (`detail()` in `TaskDetailPanel.test.tsx`, `taskDetail()` in `ActiveRunCard.test.tsx`,
  `taskDetailFor()` in `RunsView.test.tsx`, and any a later task added) gains the line
  `worktreeRunner: null`.

**Screenshots and the rest:**

- `npm run screenshot` produces every capture that existed before this task, unchanged,
  plus the seven new captures in each of its four projects: dark and light, 1440 and 1024.
  The run that implemented this looked at them, and the PR names the files for the sign-in
  screen, the switcher, a task on another machine, the review queue in the browser, the
  runners section and the account page, in both schemes, and says what was changed after
  looking.
- The seam entry of Scope 8 exists, in the four-part shape, and "How to use this" has a
  050 row.
- CLAUDE.md has Scope 9's lines, and `## Commands` is unchanged.
- `Cargo.toml` gains only `tower-http`'s `cors` and `fs` features (D34). `package.json`
  gains nothing. `cargo tree -d` shows one `axum`.
- The worktree-runner query is in the board cache, regenerated with D33's recipe for it.
  No migration is added.
- **Every CI check passes:** `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, the server crate's tests as 046 added them to CI,
  `cargo fmt --all --check`, `cargo clippy` over every crate CI lints, with
  `--all-targets -- -D warnings`, `cargo check --workspace --all-targets`, and
  `./scripts/check-command-wiring.sh`.
- **Needs a person, listed in the PR as a checklist:** sign in through a real GitHub OAuth
  app against a local server serving `dist/`; decline at GitHub once and see the `denied`
  sentence; switch between two teams; revoke the session from a second browser and watch
  the first return to sign-in; deploy a newer server behind an open tab and see one reload.

## Notes

**Seam entries to read:** **D32** in full, especially point 6 (`board<T>`/`local<T>` and
the browser refusal), point 7 (`Caller`, doors, the cookie and header names, the
`Rimaia-Protocol` rule and "050 reloads"), and the appendix, which says which commands are
local and which task flips each one. **"Task 048's cross-cutting choices"** (the
revalidation closure that re-applies `narrow_to`, the opening burst, `?tail=`), and
**D14**'s 048 amendment (`LiveEvent` carries its team). **D34**
(`tower-http`'s `cors` and `fs` are this task's; nothing else is added). **D28** part 6:
`runners` (038), 045's `eligibility`, and 047's `sessions`, `api_tokens` and
`pairing_codes`. **D29** point 4 (why `worktreeRunner` takes every kind). **D33** (which
cache a board query regenerates). Also D7 (the two frontend modules that own the
boundary), D8 (no new error code: role refusals are `invalid`, a foreign team is
`not_found`), D11 (a misconfigured web root fails loudly), D12 (the bulk read does not
change), and D22 (what a doctor status means, for the sentence 069 replaces). Outside the
seam contract, read **ADR-0034's 2026-10-04 amendment**, which is what `RunnersSection`
implements, and **ADR-0033 point 7**, which is why the review view must work here.

**Migration:** none.

**Files to start from.** Server, from 046 to 048: `crates/server/src/` (the router, the
configuration, `caller.rs`'s extractor), `crates/server/tests/commands.rs`. Core, from 046
and 047: `crates/core/src/api/{mod,registry,caller}.rs`, `crates/core/src/api/board/`
(047's `account.rs`), `crates/core/src/identity/tokens.rs`,
`crates/core/src/testing/api.rs`, `crates/core/src/context.rs` (`for_caller`),
`crates/core/src/tasks/service.rs` (`TaskDetail` at line 44, `fetch_last_run` at line 1261
on `main`) and `crates/core/src/mcp/responses.rs` (`TaskView`). Frontend: `src/main.tsx`,
`src/App.tsx`, `src/components/Sidebar.tsx`, `src/components/board/TaskCard.tsx`,
`src/components/panel/WorktreeSection.tsx`, `src/components/DoctorBanner.tsx`,
`src/views/SettingsView.tsx`, 017's `src/views/ReviewView.tsx` and `src/lib/open.ts` with
their tests, `src/components/runs/RunReviewSections.tsx` (017's `liveDiff` prop), the
components Scope 6's gate table names,
`src/lib/format.ts`, `src/types.ts`, and whatever 049 made of `src/lib/commands.ts`,
`src/lib/events.ts` and `src/lib/client.ts`. Fixture mode (028): `src/dev/fixtures/`,
`screenshots/views.shot.ts`.

**What earlier tasks provide.**

- **046:** the server crate and router, the registry, `Caller` and its extractor, the
  `/api/v1` JSON fallback, `Rimaia-Protocol` and `upgrade_required`, and `BOARD_CASES`.
- **047:** `Authenticate` over sessions and hashed tokens, `/auth/github` and the
  `?sign_in=` vocabulary, the CSRF cookie, a personal team for every new user, the eight
  account rows (`get_account`, `list_sessions`, `revoke_session`, `list_api_tokens`,
  `create_personal_access_token`, `revoke_api_token`, `create_pairing_code`,
  `pair_own_runner`), and `identity::tokens::unpair_runner`. If one of those is missing,
  stop: it is 047's to add, not 050's to improvise.
- **048:** `/api/v1/events` filtered by `caller.teams`, and the revalidation closure that
  re-applies `narrow_to`.
- **049:** the HTTP and SSE transports, `transportHeaders`, `bootstrapClient()`,
  `get_client_capabilities` and its browser answer, `subscribeToEventStreamFailure`, the
  local refusal, and the HTTP test mock. The capability field names are 049's.
- **038, 045:** `runners` with `label`, `provider`, `app_version`, `last_seen_at` and
  `eligibility`, and `runs.runner_id`.
- **017, with 033 and 037:** `ReviewView` reading `get_task` for the task on screen, its
  `w` and `o` keys, `openExternalUrl` as the only importer of `@tauri-apps/plugin-opener`,
  and `RunReviewSections`' `liveDiff` prop. 017's Notes expect 049 to switch `open.ts` to
  `window.open` in the browser; 049 does not, and this task does it (Scope 6). If 017
  landed with different names, theirs win.

**What later tasks expect.**

- **051:** a switcher and an account page to add team management and invitations to, and
  `ignores_team_header`, to which it adds its rows.
- **052 and 056:** each deletes its rows' gates from Scope 6 when it flips the command,
  except Run now and Retry, whose gates 069 deletes with its runner picker. 054 and 060
  delete no gate: Add repository's command stays local, and so do the planning commands.
  052 and 060 each test that their surface ignores `Rimaia-Team`.
- **054:** adds `list_runner_doctor_reports`; **069** renders it in `RunnersSection`, and
  builds the browser form to register a repository by remote URL.
- **057:** pin release in 047's `unpair_runner`, which this task's command calls.
- **058:** the pairing line, printed exactly.
- **059:** runs this shell in connected mode, with `worktreeIsHere` telling its own
  worktrees from others in the panel and the review view, supplies its own sign-in
  action, and amends D12 if it wants the card's Open in… back.
- **060:** the MCP line next to the token dialog, and a `list_teams` tool paired with the
  command.
- **061:** extends the runner rows with assignment and eligibility controls.
- **069 and 058:** cite this task for "the board holds no runner settings". ADR-0034's
  2026-10-04 amendment is now where that rule lives, and either may point there.
- **062:** copies `dist/` into the image and sets `RIMAIA_WEB_ROOT`.
- **063:** `list_runners` gains `currency`.

**Size.** L, and close to the limit: roughly 4,000 lines across the server, the three
commands and `worktreeRunner`, the frontend, its tests and the fixtures. The review view's
two gates, `open.ts`'s opener and scheme check, and the overlay's fallback add about 250
lines with their tests and the seventh capture, which this task absorbs rather than
splitting off: they are ADR-0033 point 7's promise, and the cut list below is the relief
valve if the whole runs over. If it runs over,
cut in this order, and amend the receiving task's file in the same commit:

1. **The Scope 6 gates beyond ADR-0034 point 5's three states**: the run controls, the
   queue status and controls, planning, the transcript reads and Add repository. They
   need no decision, and become a follow-up task placed before 052. The no-local-command
   test is then scoped to Settings and Account, and the PR says so.
2. **The token-creation dialog** moves to 060, which needs it for the MCP line anyway.
   Listing and revoking stay here.
3. **"Pair a runner"** moves to 058, which is its first user.

Never cut the header narrowing, CORS, the bundle's cache headers, the one-reload rule, the
review view's two gates or `open.ts`'s scheme check. Those are the parts that a later task
cannot see is missing, and the last is the one whose absence is a script-injection hole
rather than a broken button.
