---
id: "028"
title: Let a run see the UI it changed
milestone: v0.3
status: ready
depends_on: ["005"]
adrs: ["0015"]
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

## What makes this cheap here

Three properties this codebase already has, and a fourth that arrived with the agent:

1. **The frontend is a plain web app in dev.** `npm run dev` is Vite on `localhost:1420`.
   Rendering it needs no Tauri, no Rust and no window.
2. **Everything crosses one seam.** `src/lib/commands.ts` is the only module importing
   `invoke`, and `src/lib/events.ts` the only one importing `listen` (seam-contract D7).
   The test suite already exploits this — it mocks `@tauri-apps/api/core` and never the
   wrappers, and `StorageSection.test.tsx` explains why. The same seam works in a browser.
3. **The fixture habit exists.** Every `*.test.tsx` already has factory functions building
   a `TaskSummary`, a `Run`, a `QueueStatus`.
4. **Claude Code can read a PNG.** That is the actual enabler: the agent screenshots, looks
   at the image, and iterates — rather than reasoning about spacing it will never see.

## Scope

**1. A fixture mode for the dev server.** A dev-only entry — a query flag, a separate Vite
entry point, whatever is smallest — that installs a stubbed `invoke`/`listen` returning a
seeded workspace instead of reaching a backend. It must be impossible to ship: gate it on
`import.meta.env.DEV`, and add a test asserting the production bundle contains no fixture
data.

The seed is the point, so choose it deliberately. It needs the states someone must
actually look at, including the ugly ones:

- cards in all four columns, including a column with one card and a column with twenty
- every `RunState` — `idle`, `queued`, `running`, `blocked`, `waiting_retry`, `failed`,
  `cancelled` — plus a card whose last run was `interrupted` (seam-contract D9: that word
  is read off the run, not the state)
- a blocked card with a long blocking title, and a card with a title long enough to wrap
- one, two and three concurrent runs in the Runs view, each with a live tail
- a populated run history, and an empty one
- a doctor report with a pass, a warn and a fail
- the welcome screen
- an error banner

**2. A screenshot script.** `npm run screenshot` — start the dev server, drive a headless
browser over a list of routes × colour schemes × two viewport widths, write PNGs to a
gitignored `.screenshots/`. Deterministic file names, so a before/after pair can be
compared by opening two files rather than by hunting.

**3. Wire it into the run.** Add to the base instructions, or document it in `CLAUDE.md`'s
command list, that a run which changed anything under `src/` takes screenshots and **looks
at them** before it finishes. An agent that produced an unreadable badge should find that
out from the image, not from the user.

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
  them, and nothing in CI depends on them.

## Acceptance criteria

- `npm run screenshot` produces PNGs of the board, Runs, Settings, the doctor and the
  welcome screen, in both colour schemes, from a cold checkout with no Rust built and no
  database present.
- Every state listed in Scope 1 appears in at least one screenshot.
- The fixture data cannot reach a production build, proven by a test rather than by
  inspection.
- A deliberately broken stylesheet — the `*/`-in-a-comment bug that actually shipped —
  produces a visibly unstyled screenshot rather than a passing run.
- The base instructions or `CLAUDE.md` tell a run to look at its own screenshots, and say
  what to look for: contrast, overflow, wrapping, and whether state is distinguishable
  without colour.

## Notes

**Adding a browser is a dependency decision, not a detail.** Playwright is the obvious
candidate and it costs a devDependency plus a ~150 MB browser download. Seam-contract D6
makes that an explicit ask: **stop and ask before adding it**, and say in the PR what was
considered instead.

**Be honest about what this buys.** It reliably catches *wrong* — unreadable contrast,
overflow, a card that collapses at two columns, a badge invisible on a dark surface. It
helps less with *taste*: an agent looking at its own screenshot still grades itself
generously, and the larger lever on taste turned out to be the brief. Both UI chains had
the same tooling and produced very different work; what differed was what they were asked
for. This task removes an excuse, not the need for a good plan.

**The fixture set is the maintenance cost.** A stale fixture is an agent confidently
reviewing a screen nobody ships. Keep it small enough to stay true, and put it somewhere a
person changing `TaskSummary` will notice it.
