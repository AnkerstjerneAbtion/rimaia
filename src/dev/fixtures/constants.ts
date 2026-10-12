// Imports nothing, on purpose: `screenshots/views.shot.ts` runs under
// Playwright's Node loader, which cannot follow the seed's imports into
// `src/types.ts`, `import.meta.env` or a stylesheet, and `bundle.test.ts`
// wants these two without loading the seed it is proving absent from the
// production bundle.

/** The instant every timestamp in the seed is an offset from. The screenshot
 *  script fixes the page's clock to it, so "5m ago" renders the same every
 *  run. A string rather than a `Date` so Playwright's `setFixedTime` and the
 *  seed's `Date.parse` read it identically. */
export const FIXTURE_NOW = "2026-10-04T12:00:00.000Z";

/** Embedded in a card title of the `busy` seed. The bundle test searches the
 *  production output for it: if a fixture ever reached the bundle, this would
 *  come with it. */
export const FIXTURE_SENTINEL = "fixture-seed-sentinel";

/** The window property the fixture transports flip, and the screenshot script
 *  waits on, to know the page has stopped fetching. */
export const SETTLED_FLAG = "__rimaiaFixtureSettled";
