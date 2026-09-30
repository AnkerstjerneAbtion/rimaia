---
id: "028"
title: Let a run see the UI it changed
milestone: v0.3
status: ready
depends_on: ["005"]
adrs: ["0015", "0024", "0034"]
size: M
---

# Let a run see the UI it changed

## Goal

Give an unattended run a way to **look at the interface it just edited** — render the app,
screenshot it, and read the image back — so a UI task is judged against what it renders
rather than against what its author imagined.

## Why now

Two chains of UI tasks have run through this repository and the evidence is unambiguous.

The first was told to be conservative about anything it could not verify. It produced 252
`var(--token)` substitutions in `board.css` and **two** lines of actual layout or visual
effect — a change nobody could see. The second was told the opposite, and did move the
design; but its own PRs still had to say things like *"you cannot see the result"*, and the
one bug that reached a running app was a CSS comment containing `*/` that silenced an
entire stylesheet. Nothing in the gate looked at a pixel, so nothing caught it.

Both failures share a cause. An agent editing a stylesheet is working blind, and the
current gate — `typecheck`, `vitest`, `build` — proves the code compiles and the DOM is
correct. **None of it proves the screen is legible.** jsdom has no layout engine
(`src/lib/board.ts` says so where it explains why the pure logic was extracted), so no
amount of testing at that level ever will.

This is not only Rimaia's problem. Every repository Rimaia runs against that has a
frontend has the same gap, and this task is the smallest thing that closes it here first.

It is also first in line now for a reason of its own. Most of the backlog after it — the
morning review (017), the review loop's interface (037), the web shell (050), the consent
and runner screens (061) — is UI work that will run unattended. Every one of those runs
inherits this task's ability to see, or works blind the way the two chains above did.

### What makes this cheap here

Three properties this codebase already has, and a fourth that arrived with the agent:

1. **The frontend is a plain web app in dev.** `npm run dev` is Vite, the same React, CSS
   and DOM the window shows. Rendering it needs no Tauri, no Rust and no window.
2. **Everything crosses one seam.** `src/lib/commands.ts` is the only module importing
   `invoke`, and every command goes through its private `call<T>()`; `src/lib/events.ts`
   is the only one importing `listen` (seam-contract D7). The test suite already exploits
   this — 31 test files mock `@tauri-apps/api/core` and never the wrappers, and
   `StorageSection.test.tsx` explains why. The same seam works in a browser.
3. **The fixture habit exists.** Every `*.test.tsx` already has factory functions building
   a `TaskSummary`, a `Run`, a `QueueStatus`.
4. **Claude Code can read a PNG.** That is the actual enabler: the agent screenshots, looks
   at the image, and iterates — rather than reasoning about spacing it will never see.

## Scope

**1. A transport seam under `call<T>()` and the event wrappers.** The stub is not a
second copy of the wrappers and not a mock of `@tauri-apps/api/core`: it is a replaceable
transport *beneath* the two modules that already own the boundary. ADR-0034 point 4 says
those same two modules will later pick a transport per command kind and mode ("`commands.ts`
and `events.ts` get a transport, and nothing else changes"); this task builds the place
that choice will be made, with one alternative in it, so task 049 adds implementations to
it rather than replacing it.

- **`src/lib/commands.ts`** exports `type CommandTransport = (command: string, args?:
  Record<string, unknown>) => Promise<unknown>` and `setCommandTransport(transport)`. The
  default is Tauri's `invoke`, looked up **at call time**, so the 31 test files that mock
  `@tauri-apps/api/core` today keep intercepting every call without an edit. `call<T>()` keeps
  its name, its signature and its `toRimaiaError` normalisation, and sends through the
  installed transport — so a fixture's refusal reaches a component exactly as a backend
  error does. `scripts/check-command-wiring.sh` parses `call<T>("name"` literals; that
  shape does not change.
- **`src/lib/events.ts`** exports `type EventTransport = <P>(event: string, onPayload:
  (payload: P) => void) => Promise<UnlistenFn>` and `setEventTransport(transport)`. Every
  subscribe wrapper goes through one private `subscribe<P>()` instead of calling `listen`
  itself; the default wraps `listen` and unwraps `event.payload`, so a transport hands over
  the payload and never a Tauri event envelope. The wrappers' exported signatures do not
  change.
- **Two setters, not one object**, because D7 splits the imports: `invoke` may appear only
  in `commands.ts` and `@tauri-apps/api/event` only in `events.ts`, so each half of the
  Tauri transport lives where its import already is. 049's HTTP transport lands in
  `commands.ts` and its SSE client in `events.ts` (D34 confines
  `@microsoft/fetch-event-source` to that file), which is the same split.
- **No component calls a setter.** Only the fixture entry does, today; 049's bootstrap
  will, later.

**2. A fixture mode for the dev server.** A separate Vite entry, `fixtures.html` at the
repository root, loading `src/dev/main.tsx`, which installs a fixture `CommandTransport`
and `EventTransport`, then renders `App` unchanged. It must be impossible to ship:

- `vite build`'s input stays `index.html` alone, so nothing under `src/dev/` is in the
  production module graph;
- `src/dev/main.tsx` throws before installing anything unless `import.meta.env.DEV`;
- a test builds the production bundle and asserts no fixture reached it (acceptance
  criteria).

The fixture transport answers from a table in `src/dev/fixtures/`, keyed by command name,
typed against `src/types.ts`, so a change to `TaskSummary` that the seed does not follow
fails `npm run typecheck`. Every command `commands.ts` sends has a row: a seeded answer,
or an explicit refusal with a reason (`debug_provoke_error` has nothing to show). A name
with no row rejects with `{ code: "internal", message: "fixture mode has no answer for
\`<name>\`" }` rather than hanging — a stale fixture should be loud. Every refusal, that one
and the explicit ones alike, uses a code `ErrorCode` already has — `invalid` when a user
could act on it, `internal` when not — because D8 says the error type does not grow and a
fixture that invented a code would render a banner no backend can produce. **Writes answer
without changing the seed.** A fixture is a picture, not a backend; nothing the screenshot
script does clicks a mutating control.

The seed is the point, so choose it deliberately. It needs the states someone must
actually look at, including the ugly ones. It is organised as named scenarios, chosen by
`?scenario=<name>`; an unknown name renders the list of valid ones instead of the app:

| Scenario | What it seeds |
| --- | --- |
| `busy` | cards in all four columns, one column holding **one** card and another **twenty**; every `RunState` — `idle`, `queued`, `running`, `blocked`, `waiting_retry`, `failed`, `cancelled` — plus a card whose last run was `interrupted` (D9: that word is read off the run, not the state); a blocked card with a long blocking title (D12's blocker name), and a card with a title long enough to wrap; three concurrent runs in the Runs view, each with a live tail; a populated run history; analytics with data; a doctor report with a pass, a warn and a fail (D22's statuses), plus a second warn that is dismissed (D22's 2026-09-04 amendment: marked, never dropped); `get_run_capacity` and `get_queue_status` consistent with three in flight — `mode: "parallel"`, `maxConcurrency` of at least 3, the three running ids in `runningTaskIds`, and repository caps that admit them (D21) |
| `one-run` / `two-runs` | the Runs view with exactly one and exactly two concurrent runs, each with a live tail; capacity and queue status consistent with that count, as in `busy` (`two-runs` is `parallel`) |
| `empty` | a registered repository, an empty board and an empty run history |
| `welcome` | `onboardingDismissed: false`, so `App` opens on the welcome screen |
| `error` | the Runs view's first read rejects with a realistic `RimaiaError`, so the error banner renders |

A live tail is the fixture `EventTransport` delivering canned `runs:tail` payloads for each
running run once it is subscribed, alongside the `get_run_tail` answer that D14's catch-up
reads. Every timestamp in the seed is an offset from one exported constant, `FIXTURE_NOW`,
which the screenshot script also uses to fix the page's clock.

`FIXTURE_NOW` and `FIXTURE_SENTINEL` (the string the `busy` seed embeds in a card title, which
the bundle test searches for) live in **`src/dev/fixtures/constants.ts`, a module that imports
nothing.** The seed imports them from there, and so do `screenshots/views.shot.ts` and
`bundle.test.ts`. The spec runs in Playwright's Node loader, which cannot follow the seed's
imports into `src/types.ts`, `import.meta.env` or a stylesheet; a constant defined in the seed
would drag all of that in with it.

**3. A screenshot script.** `npm run screenshot` — start the dev server, drive a headless
browser over a list of views × colour schemes × two viewport widths, write PNGs to a
gitignored `.screenshots/`. Deterministic file names, so a before/after pair can be
compared by opening two files rather than by hunting.

- **Playwright, WebKit only**, as D34 approves: `@playwright/test` `^1` as a
  devDependency, the engine installed once with `npx playwright install webkit`. WebKit is
  the engine closest to WKWebView, which is what ships on macOS (ADR-0002).
- **Its own port, never 1420.** `npm run tauri dev` holds 1420 with `strictPort`, and
  only one worktree at a time can. `npm run screenshot` runs `scripts/screenshot.mjs`,
  which asks the operating system for a free port (listen on `0`, read it, close), passes
  it to `playwright.config.ts` as `RIMAIA_SCREENSHOT_PORT`, and starts `playwright test`
  with an argument vector, not a shell string. The config's `webServer` starts `vite
  --port <port> --strictPort` and waits for it, with **`reuseExistingServer: false`** —
  reusing a server already on that port would screenshot another worktree's code and say
  nothing. Two worktrees running the script at the same moment both succeed.
- **No database, no Rust.** The fixture transport answers every command, so `target/`,
  `RIMAIA_DATA_DIR` and the data directory are never read or needed.
- **Deterministic pixels.** WebKit only; `locale: "en-US"`, `timezoneId: "UTC"`; the page
  clock fixed to `FIXTURE_NOW` with `page.clock.setFixedTime` before navigation. Two more
  things are needed, because the `busy`, `one-run` and `two-runs` scenarios seed running
  cards and those carry infinite animations (`status-pulse` in `src/styles.css`,
  `active-run-sweep` in `src/styles/runs.css`), and because the fixture `EventTransport`
  delivers its `runs:tail` payloads asynchronously once a subscriber exists:
  - **Captures are `page.screenshot({ path, fullPage: true, animations: "disabled" })`.**
    `reducedMotion` stays at its default, so the frame captured is the one that ships, not
    the reduced-motion variant.
  - **A settle signal.** The fixture transports keep a count of outstanding work — command
    answers not yet resolved and canned events not yet delivered to a live subscriber. They
    set `window.__rimaiaFixtureSettled = false` synchronously whenever work arrives, and
    `true` one macrotask after the count reaches zero. After navigating and clicking to the
    view, the spec waits for the view's landmark to be visible, then for that flag, then for
    `document.fonts.ready`, and only then captures. No fixed wait stands in for any of these.

  **Never `toHaveScreenshot`** — that is golden images, which Out of scope rules out.
- **The matrix.** Playwright `projects` express `colorScheme` (`dark`, `light`) × width
  (1440 and 1024, both 900 tall — ADR-0024 expects widths the desktop window never sees).
  The view list is a table in the spec; a view is reached by clicking the sidebar, because
  `App` has route state, not URLs (see its own comment):

  | Scenario | Views |
  | --- | --- |
  | `busy` | board, runs, analytics, settings (its doctor section) |
  | `one-run`, `two-runs` | runs |
  | `empty` | board, runs |
  | `welcome` | welcome |
  | `error` | runs |

  That is 11 captures per project and 44 per run.
- **File names:** `.screenshots/<label>/<scenario>--<view>--<scheme>--<width>.png`, e.g.
  `.screenshots/latest/busy--board--dark--1440.png`. The label is `latest` unless
  `npm run screenshot -- --label before` names another; a label beginning with `.` is refused,
  so no label can collide with Playwright's directory below. Any other argument is passed to
  `playwright test` (`-- --grep runs` narrows the set). **Only an un-narrowed run clears its
  label's directory first**; a narrowed run overwrites the files it writes and leaves the
  rest, so retaking one view does not delete the other 43. Either way a run touches only its
  own label, so "before" survives the "after".
- **The config pins what Playwright would otherwise guess.** `testDir: "screenshots"` and
  `testMatch: "*.shot.ts"`, because Playwright's default collects every `*.{spec,test}.*`
  under the config's directory, which is the whole vitest suite in `src/`. And a fixed
  `outputDir: ".screenshots/.playwright"`, because Playwright empties its `outputDir` at the
  start of every run and must never be pointed at a label. Everything is under
  `.screenshots/`, so one `.gitignore` line covers it.
- **A crash is not a screenshot.** The spec fails, naming the file, when the page raises an
  uncaught error or shows Vite's error overlay. It still writes the PNG, so the failure can
  be looked at. This asserts that the page rendered, never what it looks like.
- **Where the files live.** `playwright.config.ts` at the root and the spec as
  `screenshots/views.shot.ts`: outside `tsconfig.json`'s `include`, as `vite.config.ts`
  already is, because type-checking them would need `@types/node`, which D34's closed list
  does not approve. The `.shot.ts` suffix keeps vitest's default `*.{test,spec}.*` include
  from collecting a Playwright file into `npm run test`. The spec imports nothing from `src/`
  except `src/dev/fixtures/constants.ts` (Scope 2).

**4. Wire it into the run.** A run which changed anything under `src/` takes screenshots
and **looks at them** before it finishes. An agent that produced an unreadable badge should
find that out from the image, not from the user.

This goes in `CLAUDE.md`, **not** in the base instructions. Base instructions are composed
into every run against every repository (ADR-0009), and `npm run screenshot` exists only in
this one; a repository's own commands belong in that repository's `CLAUDE.md`, which a run
in its worktree already loads. And it goes in its own short section after `## Commands`,
not inside that block: the block says it is exactly what CI runs, and CI does not run this.

## Out of scope

- **Visual regression testing.** No golden-image diffing, no snapshot approvals, no
  threshold tuning. This task is about an agent *seeing* its work, not about failing a
  build on a two-pixel shift — and a golden-image suite is a maintenance burden that
  earns its place only once the design has stopped moving.
- **Driving the real Tauri window.** `tauri-driver` has no macOS support, which is the
  platform ADR-0002 targets first. The dev server is the whole point: it is the same
  React, the same CSS and the same DOM, and the parts it cannot show (native menus, the
  window chrome) are not what a UI task is changing.
- **Testing behaviour through the browser.** ADR-0015 says no E2E, and this does not
  become one. The screenshots are for a human or an agent to *look at*; nothing asserts on
  them beyond "the page rendered", and nothing in CI depends on them. `ci.yml` does not
  change, and no CI job installs a browser.
- **A stateful fake backend.** Writes do not change the seed; a drag, a save or a queue
  start in fixture mode shows nothing new. Screenshots of a sequence are a later ask.
- **Any transport but Tauri's and the fixture's.** The HTTP transport, the SSE client, the
  `board`/`local` split of `call<T>()` (D32 point 6, task 046) and
  `get_client_capabilities` belong to 046 and 049. This task leaves the seam they fill,
  not a sketch of them.
- **Sharing factories with the test suite.** The existing tests' local factories stay
  where they are; extracting them is a refactor this task does not need.

## Acceptance criteria

- **The seam, in two new files, `src/lib/transport.test.ts` and `src/lib/events.test.ts`.**
  `src/lib/commands.test.ts` is not touched; the command-seam cases go beside it, not in it:
  - `it("sends every command through the installed transport, not invoke")`
  - `it("answers through Tauri's invoke when no transport is installed")`
  - `it("normalises a transport's rejection into a RimaiaError")` — a bare string and an
    `Error` both arrive as `{ code: "internal", message }`, as `toRimaiaError` promises.
  - `it("hands a subscriber the payload, not the Tauri event envelope")`
  - `it("routes every subscribe wrapper through the installed event transport")`
- **Every existing test file passes without an edit**, the 31 that mock
  `@tauri-apps/api/core` included. `git diff --stat` across task 028's own commits — from
  the commit it started on to its `Task: 028` landing commit, not the whole shared PR —
  shows no change to any test file that existed before it.
- `commands.ts` is still the only non-test module importing `@tauri-apps/api/core` and
  `events.ts` the only one importing `@tauri-apps/api/event`; no file under `src/components/`,
  `src/views/` or `src/hooks/` calls `setCommandTransport` or `setEventTransport`; and
  `./scripts/check-command-wiring.sh` passes unmodified.
- **The fixture, in `src/dev/fixtures/fixtures.test.ts`:**
  - `it("has an answer or an explicit refusal for every command commands.ts sends")` — it
    reads `src/lib/commands.ts` with `import source from "../../lib/commands.ts?raw"` and
    extracts every `call<…>("name"` literal (the same shape the wiring script parses), so a
    wrapper added without a fixture row fails. The `?raw` import is typed by `vite/client`,
    which `src/vite-env.d.ts` already references; a `node:fs` read would fail
    `npm run typecheck` without `@types/node`, which D34 does not approve.
  - `it("never reaches invoke or listen in fixture mode")` — with the fixture transports
    installed, every wrapper is called once and the mocked `invoke` and `listen` record
    zero calls.
  - `it("seeds every state the scope lists")` — against the seed data, not a screenshot:
    every `RunState`, all four columns, one column with exactly one card and one with
    twenty, an `interrupted` last run, a blocker title and a card title of at least 80
    characters, one/two/three concurrent runs across the scenarios, a doctor report with
    `pass`, `warn` and `fail` and a dismissed `warn`, an empty run history,
    `onboardingDismissed: false`; and in every scenario with runs in flight, a capacity and
    queue status that admit exactly that many (`runningTaskIds` matches the running runs).
- **The fixture cannot reach a production build, proven by a test:**
  `src/dev/fixtures/bundle.test.ts` (`// @vitest-environment node`),
  `it("keeps every fixture out of the production bundle")`, runs Vite's `build()` with
  `mode: "production"` against the real config with `write: false`, and asserts that no
  output chunk contains `FIXTURE_SENTINEL` or a module id under `src/dev/`. vitest sets
  `NODE_ENV=test`, and Vite derives `import.meta.env.PROD` — and React its production
  build — from `NODE_ENV`, so the test sets `process.env.NODE_ENV = "production"` for the duration of
  the build and restores the previous value in a `finally`, reaching `process` through
  `vite.config.ts`'s existing `// @ts-expect-error process is a nodejs global` idiom. A full
  build outruns vitest's 5 s default, so the test declares its own timeout (120 s).
  Separately, `src/dev/main.tsx` throws unless `import.meta.env.DEV`.
- **`npm run screenshot` produces all 44 PNGs** — board, Runs, Analytics, Settings with
  its doctor section, and the welcome screen, in both colour schemes and both widths, named as
  Scope 3 gives — from a fresh clone after `npm ci` and `npx playwright install webkit`,
  with no `target/` directory and `RIMAIA_DATA_DIR` unset.
- **It never touches 1420.** It succeeds while another process listens on `127.0.0.1:1420`
  (a one-line `node -e` server is enough; nothing needs to build the Tauri shell), and two
  invocations started together in two checkouts of the same commit (`git worktree add`)
  both succeed.
- **Running it twice without a code change produces byte-identical PNGs**, checked with
  `shasum` over `.screenshots/latest/`. This is what makes a before/after pair a
  comparison rather than noise.
- **A deliberately broken stylesheet produces a visibly unstyled screenshot**, with a
  before/after pair taken with the `*/`-in-a-comment bug that actually shipped planted in
  `src/styles.css`. The sabotage is not committed.
- **The evidence is in the commit body**, not a PR description — this branch has one PR for
  the whole backlog, and the workflow's land step writes commits. The body of task 028's
  final commit, or of its landing commit, records the fresh-clone run above, the `shasum`
  comparison, and the before/after pair by file name with what differed.
- `npm run test` does not collect `screenshots/views.shot.ts`, `npm ci` downloads no
  browser, and `.github/workflows/ci.yml` is unchanged.
- `package.json` gains exactly one devDependency, `@playwright/test` `^1`, and one script,
  `screenshot`; `.gitignore` gains `.screenshots/`.
- **`CLAUDE.md` tells a run to look at its own screenshots**, in a section of its own after
  `## Commands`, and says what to look for: contrast, overflow, wrapping, and whether state
  is distinguishable without colour — in both colour schemes and at both widths. It names
  the one-time `npx playwright install webkit`, the `--label` flag for a before/after pair,
  and the rule that a new command needs a fixture row.

## Notes

**Seam entries to read** (the task's row in `docs/seam-contract.md`'s "How to use this"
table): D6 and its 2026-09-30 amendment, then **D34** (the Playwright
approval, WebKit only, its own port, nothing in CI — this entry *is* the ask the old
version of this task required, answered); **D7** (the two modules that own the boundary);
**D32 point 6** (046 splits `call<T>()` into `board<T>()`/`local<T>()` and 049 gives them
their transports — read it so the seam built here is the one they fill); D8 (the
`ErrorCode` set a fixture refusal must stay within); D9 (`interrupted` is read off the run);
D12 and its amendments (what `TaskSummary` carries: strategy, blocker, `resume_after`); D14
(the tail's catch-up read and live event); D21 (what capacity and queue status must say for
three runs to be in flight at once); D22 and its 2026-09-04 amendment (what a doctor status
means, and how a dismissed warn is marked rather than dropped). **No migration**, and no
Rust changes.

**Files to start from:** `src/lib/commands.ts` (`call<T>()` and `toRimaiaError`),
`src/lib/events.ts`, `src/main.tsx`, `src/App.tsx` (how the opening view is chosen),
`vite.config.ts` (the 1420 `strictPort` this must not inherit), `vitest.config.ts`,
`src/lib/commands.test.ts` (how the suite mocks `invoke`; read it, do not edit it),
`src/vite-env.d.ts` (the `vite/client` types the `?raw` import relies on), the
`status-pulse` and `active-run-sweep` animations in `src/styles.css` and
`src/styles/runs.css` (what `animations: "disabled"` stills),
`src/views/RunsView.tsx` and `src/components/runs/ActiveRunCard.tsx` (the tail, and a
`Date.now()` ticker the fixed clock tames), `src/hooks/useDoctor.ts`,
`src/components/ErrorBanner.tsx`, `scripts/check-command-wiring.sh` (the `call<` shape the
fixture-coverage test mirrors).

**Previous and next.** 005 gave the board, and everything landed since gave the other
views this seeds. Next in order is 033, which is not UI; the first task to use this is
017. From here on, **every task that adds a command adds its fixture row**, or the coverage
test fails — that is the intended pressure, and the reason the rule goes in `CLAUDE.md`.
046 renames `call<` to `board<`/`local<`; it updates the coverage test's extraction in the
same commit it updates the wiring script's, and both keep going through
`CommandTransport`. 049 adds the HTTP `CommandTransport` and the SSE `EventTransport`
beside the fixture ones and chooses between them by command kind and mode; it does not
remove fixture mode. 050 may want a browser-mode scenario (capabilities saying "no local
runner"); that is a row and a scenario, not a new mechanism.

**Size.** About 1,500–2,500 lines, most of it seed data, which is M and one session. If it
runs long, cut in this order: the `one-run`/`two-runs` scenarios (the `busy` three still
show concurrency), then the `error` scenario, then `--label`. Never cut the seam, the
bundle test or the coverage test — those are what keep the fixture true after this task.

**Be honest about what this buys.** It reliably catches *wrong* — unreadable contrast,
overflow, a card that collapses at two columns, a badge invisible on a dark surface. It
helps less with *taste*: an agent looking at its own screenshot still grades itself
generously, and the larger lever on taste turned out to be the brief. Both UI chains had
the same tooling and produced very different work; what differed was what they were asked
for. This task removes an excuse, not the need for a good plan.

**The fixture set is the maintenance cost.** A stale fixture is an agent confidently
reviewing a screen nobody ships. Keep it small enough to stay true. Typing it against
`src/types.ts` and failing the suite on a missing command row are what make a person
changing `TaskSummary`, or adding a command, notice it.
