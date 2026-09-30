---
id: "050"
title: The web shell
milestone: v0.5
status: ready
depends_on: ["049"]
adrs: ["0034", "0030", "0024"]
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
- **browser states for every local-only action** (ADR-0034 point 5): the name of the
  machine that holds the worktree instead of Open worktree, runner settings shown read-only
  for each of the user's runners, and the doctor as each runner's last report instead of a
  local check.

Solo mode does not change. It has no sign-in, no account page and no switcher (ADR-0030
point 7, ADR-0029 point 2), and every existing test passes without an edit.

## Why now

049 gave `commands.ts` and `events.ts` their HTTP and SSE transports and the browser its
fixed `get_client_capabilities` answer. The browser can therefore reach the server, but
what it reaches is a desktop app with the desktop removed:

- `App.tsx` opens by calling `get_app_info`, a local command, and `DoctorBanner` runs
  `run_doctor` on every view. In a browser both are refused.
- `OpenInMenu.tsx`, `WorktreeSection.tsx`, half of Settings and the Runs view's queue
  controls call local commands and render a refusal where an answer should be.
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
  in Scope 5 would fetch the stale bundle it is trying to replace.
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
  `npm run tauri dev` (059) can reach a local server. The server passes
  `cfg!(debug_assertions)`. A release build never allows the dev origin.
- **The layer:** `CorsLayer` with those origins listed exactly (never a predicate and
  never `Any`), methods `GET`, `POST` and `OPTIONS`, request headers `authorization`,
  `content-type`, `rimaia-protocol` and `rimaia-team` (Scope 3), and a preflight
  `max_age` of ten minutes. **`allow_credentials` is never set.** A browser therefore never
  sends the `rimaia_session` cookie cross-origin, so a Tauri origin can authenticate only
  with a bearer token. The cookie and the `X-Rimaia-CSRF` header stay same-origin only.
  `x-rimaia-csrf` is deliberately not an allowed header.
- It applies to `/api/v1/*`, including `/api/v1/events`. It does not apply to the bundle,
  and it does not apply to `/api/v1/runner/*`, whose callers are not browsers.

**3. The team switcher's wire: a `Rimaia-Team` request header.** This is the one decision
here that no ADR or seam entry already makes, so this task writes it into the seam
contract (Scope 8).

- **What it means.** On the two surfaces in D32 point 7's first row, `POST
  /api/v1/<board command>` and `GET /api/v1/events`, a `Rimaia-Team: <team id>` header
  narrows the request's `Caller` to that one team. The narrowing happens once, at the edge,
  in core: `Caller::narrow_to(&self, team_id: &str) -> Result<Caller>` in
  `crates/core/src/api/caller.rs`. It returns the caller with `teams` reduced to that one
  grant, and `Error::not_found` when the caller has no grant for it, with the same message
  a missing team gets. The server's `Caller` extractor calls it after `Authenticate`. The
  extractor holds no rule of its own (ADR-0006).
- **What it does not touch.** `/mcp` never reads it: ADR-0035 point 2 rejects an implicit
  current team for agents, and 060 gives tools a `team` argument instead. The runner
  protocol never reads it: a lease determines the team (ADR-0029 point 5). The solo
  shell's `invoke` has no headers, and its caller already has exactly one team.
- **Why a header and not an argument.** Every entity-less board command would otherwise
  need a new `team` field. The invoke payloads that 24 frontend test files assert on
  would change, and D32 point 1 requires the wire shapes to be identical on both
  transports. A header is transport metadata, as `Rimaia-Protocol` is. It is sent on
  every request and stored nowhere on the server, so it is not the "current team stored per
  token" that ADR-0035 rejects. The person sees which team they are in on every screen.
- **Commands about the person, not the team, ignore it.** They read `caller.user_id`,
  never `caller.teams`. These are `list_teams`, `list_runners` and `unpair_runner` below,
  and 047's account commands. So the switcher can list every team while narrowed to one.

**4. Three board commands, only if 047 did not already add them.** Each is a board row in
`api::registry` with a core handler under `crates/core/src/api/board/`, a `board<T>`
wrapper in `commands.ts`, a fixture row (028), and a case in 046's
`crates/server/tests/commands.rs`. If 047's registry already has a command with the
meaning below, under any name, use it and add nothing.

| Command | Effect | Answers |
| --- | --- | --- |
| `list_teams` | Read | The caller's memberships, read by `user_id` from `team_memberships` joined to `teams`: `{ id, name, role, personal }`, where `personal` is `teams.personal_user_id = caller.user_id`. Personal team first, then by name |
| `list_runners` | Read | The caller's own runners (`runners.user_id = caller.user_id`, `unpaired_at IS NULL`): `{ id, label, provider, appVersion, eligibility, pairedAt, lastSeenAt }`. Never another user's runner, even one in a shared team: a runner works for exactly one user (ADR-0030 point 5) |
| `unpair_runner` | Write | `{ id }`. Sets `unpaired_at` from the injected `Clock` and deletes the runner's `api_tokens` rows, in one transaction. The row itself stays, because runs keep naming it (D28). Another user's runner, or one already unpaired, is `not_found` |

`list_teams` is ADR-0035 point 2's `list_teams`, so 060 can pair the tool with it.
Releasing an unpaired runner's pins is ADR-0031 point 4, and 057 adds it to this command.
No new `ChangeEvent`: the account page re-reads after its own write, and D8 and ADR-0018's
event list do not grow for a page that one person looks at.

**5. The machine that holds the worktree** (ADR-0034 point 5, ADR-0033 point 3).

- `TaskDetail` gains `worktreeRunner: { id, label, unpaired } | null`, mirrored in
  `src/types.ts`. It is read in `fetch_last_run`'s query
  (`crates/core/src/tasks/service.rs`) as `runs.runner_id` joined to `runners`. That
  reader takes every kind (D29 point 4), and so does this field: implementation, review
  and fix runs all work in the task's one worktree (ADR-0005). A task with no run is
  `null`. D12's bulk read does not change.
- **The rule, in one place:** `src/lib/worktreeHere.ts`,
  `worktreeIsHere(task, capabilities): boolean`. It is true in solo mode. Otherwise it is
  true only when the capabilities name a local runner and that runner is
  `task.worktreeRunner.id`. `OpenInMenu.tsx` and `WorktreeSection.tsx` call it, and the
  card's Open in… action goes through `OpenInMenu`. When it is false they render
  "Worktree on {label}" (with "(unpaired)" when the runner is unpaired), and call no
  local command. That covers the browser and the connected desktop whose runner is not the
  one that ran the task.

**6. The shell in the browser.** Every choice below comes from 049's
`get_client_capabilities` answer (its mode, local runner and local actions). Nothing
checks for a platform: no `isTauri`, no `window.__TAURI__`, no user-agent sniffing
(ADR-0034 point 5).

- **Sign-in.** When the mode is not solo, `App.tsx` first reads 047's current-user
  command. `unauthenticated` renders `src/views/SignInView.tsx`: the product name, one
  sentence, and a "Sign in with GitHub" link to 047's sign-in route. It is a plain link,
  not a `fetch`, because OAuth needs a top-level navigation. From then on, any
  `unauthenticated` from any command or from the events stream returns to that screen.
  049's transport already surfaces the code, and this task adds the one place that reacts
  to it, in `src/lib/`, so no component handles it.
- **`upgrade_required` reloads the page once** (D32 point 7, "050"). This applies only in
  browser mode. The HTTP transport records the reload in `sessionStorage` under
  `rimaia.upgradeReload` and calls `location.reload()`. If the flag is already set, it
  does not reload again. The error then reaches the caller, and `App` shows one banner:
  "This page is older than the server, and reloading did not fix it. Try again in a
  minute." The flag is cleared by the first board response that succeeds. In the desktop
  modes, `upgrade_required` is left to 063's updater and is not reloaded.
- **No local command on the way in.** In browser mode, `App.tsx` does not call
  `get_app_info`, never opens on the welcome screen (onboarding checks this machine), and
  does not render `DoctorBanner`. The sidebar shows no app version.
- **The team switcher**, `src/components/TeamSwitcher.tsx`, sits at the top of
  `Sidebar.tsx` in every mode but solo. It reads `list_teams`, shows the current team's
  name, and opens a menu only when there is more than one team.
  - **Choosing a team** sets the transport's team, stores its id in `localStorage` under
    `rimaia.team`, reconnects the events stream with the new header, and remounts the
    content area keyed by team id, so no open panel keeps an id from the team it left.
  - **At startup** a stored id that `list_teams` no longer returns falls back to the
    personal team, or to the first team if there is none.
  - **No board read is sent before a team is chosen.** The only requests before then are
    the current-user read and `list_teams`.
  - The team is state inside the module 049 put the HTTP transport in, and that module
    sends the header. No component sets a header.
- **The account page**, `src/views/AccountView.tsx`, with its sections in
  `src/views/account/`, is a sidebar entry in every mode but solo. It is built on 047's
  account commands. **The exact command names are 047's.** Use the names its task file and
  wrappers give, as 017 does with 034's.
  - **You:** login and avatar, and Sign out, which returns to the sign-in screen.
  - **Sessions:** device (the stored user agent, shortened), created and last used. The
    current session is marked, and every row has Revoke. Revoking the current session is
    signing out.
  - **Tokens:** kind, label, created, last used, where last used, and expiry, for each
    `rmd_`/`rmr_`/`rmp_` token, each with Revoke. "New personal access token" takes a
    label and, if 047's command accepts one, a subset of the user's teams (ADR-0030
    point 6). **The secret is shown once**, with a copy button and the sentence "This is
    the only time it will be shown". It is dropped from component state once the dialog
    closes. It is never written to storage, never logged, and never part of a re-read.
  - **Runners:** `list_runners`, with label, provider, version and last seen, and Unpair
    behind a confirmation naming the runner. "Pair a runner" asks 047 for a pairing code
    and shows it with its expiry and the exact line
    `rimaia-runner pair <origin> <code>`, where `<origin>` is `window.location.origin`.
- **Settings in the browser.** Sections whose commands are board commands keep working:
  instructions, strategy, repositories as the team knows them, and the subscription cost.
  Sections that exist only on a machine are replaced when there is no local runner:
  concurrency, schedules, MCP, storage, credentials, developer, run environment, and the
  doctor. They are replaced by one section, `src/views/settings/RunnersSection.tsx`,
  which shows, for each of `list_runners`:
  - the board's view of it: label, provider, version, eligibility, and last seen
    (relative, from the injected `now` that `src/lib/format.ts`'s callers already use);
  - "These settings are changed in the desktop app on {label}.";
  - **its last doctor report.** The component takes `report: DoctorReport | null` and
    reported-at, and renders `DoctorResultList.tsx` for a report and "{label} has not
    reported a doctor result yet." for `null`. The board has no report column until 054
    (D28), so this task always passes `null`. 054 adds the field to `list_runners` and
    passes it through, which is one prop. The component is still tested with a report.
- **Every other local-only control is hidden when there is no local runner.** That
  includes Run now, Retry and Cancel (local until 052), the queue controls, the live
  worktree status and diff (`get_worktree_status`, `get_diff_summary`), reveal log, remove
  worktree, the transcript reads (local until 056), and Add repository (local until 054).
  Each gate is a capability check with a one-line comment naming the command that forces
  it. The task that flips that command to `board` (D32 appendix) deletes the gate in the
  same commit.
- **Design.** ADR-0024 throughout: sentence case, no uppercase transforms, state as a dot
  and a word, one accent, fluid width, and light and dark designed equally. New styles go
  in the existing leaf stylesheets' idiom and use the existing tokens.

**7. Screenshots in both schemes** (task 028's mechanism, not a new one). The fixture
table gains a row for every command this task adds or starts calling, and its
`get_client_capabilities` answer becomes per scenario. New scenarios:

| Scenario | Seeds | Views |
| --- | --- | --- |
| `signed-out` | browser capabilities, current-user read refused `unauthenticated` | sign-in |
| `browser` | browser capabilities, two teams (personal first), the `busy` board, a task whose `worktreeRunner` is another machine and one whose runner is unpaired, three runners (one never seen) | board with the switcher open, board with a task's detail open, settings (runners section), account |
| `browser-one-team` | as `browser` with one team | board |

That adds seven captures per project, each in dark and light and at both widths, and 028's
44 stay as they were.

**8. The seam contract.** This task appends the next free D entry, "Task 050's
cross-cutting choices", in the four-part shape, recording: the `Rimaia-Team` header and
`Caller::narrow_to` (Scope 3), `RIMAIA_WEB_ROOT` and no `index.html` fallback, the cache
and framing headers, `allowed_origins` and the absence of `allow_credentials`, the three
commands and their user scoping, `TaskDetail.worktreeRunner`, and the one-reload rule. It
also adds a row for 050 to "How to use this". D32's Binds line for 050 already says
"reloads the page on `upgrade_required`", and the new entry points back to it.

**9. CLAUDE.md.** Beside 046's line on running the server, add how to run the web shell
locally: `npm run build`, then the server with `RIMAIA_WEB_ROOT` set to the absolute path
of `dist/`. `## Commands` does not change, because CI does not run this.

## Out of scope

- **Teams as things a person manages.** Creating, renaming and deleting teams,
  invitations, roles and the last-owner rule are 051's. The switcher only switches.
- **Deleting an account.** That is 051's, with team deletion (ADR-0029 point 6).
- **The `claude mcp add` line for a new personal token.** `/mcp` does not exist on the
  server until 060, and a line that connects to nothing is the bug `McpAddCommand.tsx`'s
  own comment describes. 060 adds it next to the token dialog.
- **Runner settings reported to the board.** The board holds no runner's concurrency,
  schedule or MCP port, and no D28 column exists for them. The browser's read-only view
  is the board's own facts about each runner, plus where to change the rest. Reporting
  those settings would need a D28 amendment first.
- **The doctor report itself**, its column and its reporting route. Those are 054's.
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

**CORS, in `crates/server/tests/cors.rs`:**

- `allowed_origins_are_exactly_the_tauri_origins`: `allowed_origins(false)` equals the
  three origins, and `allowed_origins(true)` equals those plus `http://localhost:1420`.
- `a_tauri_origin_may_preflight_a_bearer_request`: for each of the three origins, an
  `OPTIONS` preflight for `POST /api/v1/list_tasks` echoes the origin and allows
  `authorization`, `rimaia-protocol` and `rimaia-team`, and it carries no
  `Access-Control-Allow-Credentials`.
- `any_other_origin_gets_no_cors_headers`: `https://evil.example`, `null` and
  `http://localhost:1420` (with `dev = false`) get no `Access-Control-Allow-Origin`.
- `the_csrf_header_is_not_allowed_cross_origin`: a preflight asking for `x-rimaia-csrf`
  from a Tauri origin is not granted it.

**The team header**, as core unit tests and in `crates/server/tests/`:

- `narrowing_to_a_member_team_leaves_one_grant` and
  `narrowing_to_a_foreign_team_is_answered_as_a_missing_one`: the second compares the
  error with a never-issued id's error for equality, in the way 039's registry test does.
- `a_two_team_caller_lists_one_board_with_the_team_header`: with a two-team session,
  `list_tasks` without the header is refused `invalid` naming both teams (039), and with
  `Rimaia-Team: <A>` it returns exactly team A's tasks.
- `the_events_stream_carries_only_the_chosen_team`: on `/api/v1/events` with
  `Rimaia-Team: <A>`, a change published for team B is not delivered and one for team A is,
  read from the stream with no `sleep`.
- `mcp_and_the_runner_protocol_ignore_the_team_header`, over whichever of those routes
  exist when this lands.

**The three commands**, as core tests and as 046's per-command cases:

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
- `OpenInMenu` and `WorktreeSection` render exactly "Worktree on Studio Mac" and
  "Worktree on Old laptop (unpaired)" when the rule is false, and send neither
  `list_open_in_targets` nor `get_worktree_status`.

**The shell:**

- `it("shows sign-in when there is no session")` and
  `it("returns to sign-in when any later command is unauthenticated")`. The link's `href`
  is 047's sign-in route, exactly.
- `it("reloads once on upgrade_required, then shows the banner")`: the first
  `upgrade_required` calls the mocked `location.reload` once, a second one within the same
  session does not call it again and renders the banner's exact sentence, and a success in
  between clears the flag. `it("never reloads on upgrade_required in a desktop mode")`.
- `it("sends the chosen team on every board request and on the events stream")`: after
  choosing team B, the next `list_tasks` carries `Rimaia-Team: <B>`, and the events stream
  is reopened with the same header.
- `it("sends no board read before a team is chosen")`,
  `it("falls back to the personal team when the stored team is gone")`,
  `it("closes an open task panel when the team changes")`, and
  `it("shows the team name with no menu when there is one team")`.
- `it("renders no sign-in, account entry or switcher in solo mode")`.
- `it("shows a new token's secret once")`: after the dialog closes, the secret is in
  neither the DOM, `localStorage` nor `sessionStorage`, and a re-read of the token list
  does not bring it back.
- `it("revoking the current session signs out")`, `it("revokes a token by its id")`, and
  `it("unpairs a runner only after the confirmation")`. Each asserts the exact command and
  arguments.
- `it("shows the pairing line with this origin")`, asserting the exact string
  `rimaia-runner pair https://rimaia.example ABCD-EFGH` for the fixture's origin and code.
- `RunnersSection`: `it("says where each runner's settings are changed")`,
  `it("says a runner has not reported a doctor result")`, and
  `it("renders a runner's report through DoctorResultList")`, using a fixture
  `DoctorReport` with a pass, a warn and a fail.
- **No local command in the browser.**
  `it("sends no local command from any view in browser mode")` renders Board (with a task's
  detail open), Runs, Analytics, Settings and Account under browser capabilities. It
  asserts that no `local<T>` wrapper reached a transport and that the browser's "only
  available in the desktop app" refusal was never produced. A gate removed by mistake
  fails it.
- **Capabilities, not platforms.** `it("decides nothing from the platform")` scans
  `src/components`, `src/views`, `src/hooks` and `src/App.tsx` and finds no `isTauri`, no
  `__TAURI`, and no `navigator.userAgent`.
- **Every existing frontend test passes without an edit.** `git diff --stat` shows none of
  them changed, except that fixture coverage (028) gains the new rows.

**Screenshots and the rest:**

- `npm run screenshot` produces 028's 44 PNGs plus the seven new captures in each of its
  four projects: dark and light, 1440 and 1024. The run that implemented this looked at
  them, and the PR names the files for the sign-in screen, the switcher, a task on another
  machine, the runners section and the account page, in both schemes, and says what was
  changed after looking.
- The seam entry of Scope 8 exists, in the four-part shape, and "How to use this" has a
  050 row.
- CLAUDE.md has Scope 9's lines, and `## Commands` is unchanged.
- `Cargo.toml` gains only `tower-http`'s `cors` and `fs` features (D34). `package.json`
  gains nothing. `cargo tree -d` shows one `axum`.
- If `fetch_last_run`'s query changed, the board cache is regenerated with D33's recipe for
  it. No migration is added.
- **Every CI check passes:** `npm run typecheck`, `npm run test`, `npm run build`,
  `cargo test -p rimaia-core`, the server crate's tests as 046 added them to CI,
  `cargo fmt --all --check`, `cargo clippy` over every crate CI lints, with
  `--all-targets -- -D warnings`, `cargo check --workspace --all-targets`, and
  `./scripts/check-command-wiring.sh`.
- **Needs a person, listed in the PR as a checklist:** sign in through a real GitHub OAuth
  app against a local server serving `dist/`; switch between two teams; revoke the session
  from a second browser and watch the first return to sign-in; deploy a newer server
  behind an open tab and see one reload.

## Notes

**Seam entries to read:** **D32** in full, especially point 6 (`board<T>`/`local<T>` and
the browser refusal), point 7 (`Caller`, doors, the cookie and header names, the
`Rimaia-Protocol` rule and "050 reloads"), and the appendix, which says which commands are
local and which task flips each one. **D34** (`tower-http`'s `cors` and `fs` are this
task's; nothing else is added). **D28** part 6: `runners` (038), 045's `eligibility`,
047's `sessions`, `api_tokens` and `pairing_codes`, and 054's `doctor_report`, which does
not exist yet. **D29** point 4 (why `worktreeRunner` takes every kind). **D33** (which
cache a board query regenerates). Also D7 (the two frontend modules that own the
boundary), D8 (no new error code: role refusals are `invalid`, a foreign team is
`not_found`), D11 (a misconfigured web root fails loudly), and D12 (the bulk read does
not change).

**Migration:** none.

**Files to start from.** Server, from 046 to 048: `crates/server/src/` (the router, the
configuration, `caller.rs`'s extractor), `crates/server/tests/commands.rs`. Core, from 046:
`crates/core/src/api/{mod,registry,caller}.rs` and `crates/core/src/api/board/`, plus
`crates/core/src/tasks/service.rs` (`TaskDetail` at line 44, `fetch_last_run` at line
1261 on `main`). Frontend: `src/App.tsx`, `src/components/Sidebar.tsx`,
`src/components/board/OpenInMenu.tsx`, `src/components/board/TaskCard.tsx`,
`src/components/panel/WorktreeSection.tsx`, `src/components/DoctorBanner.tsx`,
`src/components/DoctorResultList.tsx`, `src/hooks/useDoctor.ts`, `src/views/SettingsView.tsx`
and `src/views/settings/*`, `src/components/runs/QueueControls.tsx`, `src/lib/format.ts`,
`src/types.ts`, and whatever 049 made of `src/lib/commands.ts` and `src/lib/events.ts`.
Fixture mode (028): `src/dev/fixtures/`, `screenshots/views.shot.ts`.

**What earlier tasks provide.**

- **046:** the server crate and router, the registry, `Caller` and its extractor, the
  `/api/v1` JSON fallback, `Rimaia-Protocol` and `upgrade_required`, and the per-command
  case table.
- **047:** `Authenticate` over sessions and hashed tokens, the sign-in route and callback,
  the CSRF token and how the web app gets it, a personal team for every new user, and the
  account commands: current user, sign out, sessions, tokens and pairing codes. If one of
  those is missing, stop: it is 047's to add, not 050's to improvise.
- **048:** `/api/v1/events` filtered by `caller.teams`. With the header, that filter is
  already the one-team filter.
- **049:** the HTTP and SSE transports, `get_client_capabilities` and its browser answer,
  the local refusal, and the HTTP test mock. The capability field names are 049's, and this
  file refers to them by meaning.
- **038, 045:** `runners` with `label`, `provider`, `app_version`, `last_seen_at` and
  `eligibility`, and `runs.runner_id`.

**What later tasks expect.**

- **051:** a switcher and an account page to add team management and invitations to, and
  the Scope 3 rule that a person-scoped command ignores the header. An invitation link is
  the first deep link, and it is 051's decision.
- **052, 054, 056, 060:** each deletes a gate from Scope 6 when it flips its command.
- **054:** passes `doctorReport` into `RunnersSection`.
- **057:** adds pin release to `unpair_runner`.
- **058:** the pairing line, printed exactly.
- **059:** runs this shell in connected mode unchanged, with `worktreeIsHere` telling its
  own worktrees from others.
- **060:** the MCP line next to the token dialog, and a `list_teams` tool paired with the
  command.
- **061:** extends the runner rows with assignment and eligibility controls.
- **062:** copies `dist/` into the image and sets `RIMAIA_WEB_ROOT`.

**Size.** L, and close to the limit. A rough split: server bundle, CORS and header
narrowing with tests, about 700 lines; the three commands, `worktreeRunner` and their
tests, about 600; sign-in, the switcher, the account page, the runners section and the
gates, about 1,300 of components and CSS; frontend tests, about 1,100; fixture rows and
scenarios, about 300. That is roughly 4,000 lines. If it runs over, cut in this order,
and amend the receiving task's file in the same commit:

1. **"Pair a runner"** moves to 058, which is its first user.
2. **The token-creation dialog** moves to 060, which needs it for the MCP line anyway.
   Listing and revoking stay here.
3. **The Scope 6 gates beyond the three ADR-0034 point 5 states** (Runs view controls,
   transcript reads, Add repository) become a follow-up task placed before 052. The
   no-local-command test is then scoped to Board, Settings and Account, and the PR says so.

Never cut the header narrowing, CORS, the bundle's cache headers or the one-reload rule.
Those are the parts that a later task cannot see is missing.
