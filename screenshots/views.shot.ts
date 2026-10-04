import { mkdirSync } from "node:fs";
import { join } from "node:path";

import { expect, test } from "@playwright/test";

// Imports nothing else from `src/`: this runs in Playwright's Node loader,
// which cannot follow the seed's imports (see constants.ts).
import { FIXTURE_NOW, SETTLED_FLAG } from "../src/dev/fixtures/constants";

// @ts-expect-error process is a nodejs global
const label: string = process.env.RIMAIA_SCREENSHOT_LABEL ?? "latest";

interface Capture {
  scenario: string;
  view: string;
  /** The sidebar entry to click, or `null` when the scenario opens on the view. */
  sidebar: string | null;
  /** Visible once the view has rendered. */
  landmark: string;
}

const BOARD = { sidebar: "Board", landmark: ".board-view" };
const RUNS = { sidebar: "Runs", landmark: ".runs-view" };

const CAPTURES: Capture[] = [
  { scenario: "busy", view: "board", ...BOARD },
  { scenario: "busy", view: "runs", ...RUNS },
  { scenario: "busy", view: "analytics", sidebar: "Analytics", landmark: ".analytics-view" },
  { scenario: "busy", view: "settings", sidebar: "Settings", landmark: "#settings-doctor" },
  { scenario: "one-run", view: "runs", ...RUNS },
  { scenario: "two-runs", view: "runs", ...RUNS },
  { scenario: "empty", view: "board", ...BOARD },
  { scenario: "empty", view: "runs", ...RUNS },
  { scenario: "welcome", view: "welcome", sidebar: null, landmark: ".welcome-view" },
  { scenario: "error", view: "runs", ...RUNS },
];

for (const capture of CAPTURES) {
  test(`${capture.scenario} ${capture.view}`, async ({ page }, testInfo) => {
    const scheme = testInfo.project.use.colorScheme as string;
    const width = testInfo.project.use.viewport?.width;
    const directory = join(".screenshots", label);
    const file = join(directory, `${capture.scenario}--${capture.view}--${scheme}--${width}.png`);
    mkdirSync(directory, { recursive: true });

    const crashes: string[] = [];
    page.on("pageerror", (error) => crashes.push(error.message));

    // Before navigation, so the page's first `Date.now()` is already fixed.
    await page.clock.setFixedTime(FIXTURE_NOW);
    await page.goto(`/fixtures.html?scenario=${capture.scenario}`);

    // A crash is not a screenshot, but it is still worth looking at: whatever
    // the page shows when the waits fail is written too, and the failure
    // surfaces below naming the file.
    let waitFailure: unknown;
    try {
      await expect(page.locator(".app .sidebar")).toBeVisible();
      if (capture.sidebar) {
        await page.getByRole("button", { name: new RegExp(`^${capture.sidebar}`) }).click();
      }
      await expect(page.locator(capture.landmark)).toBeVisible();
      // No fixed wait stands in for either: the flag is the fixture transports'
      // own count of answers and canned events still to be delivered.
      await page.waitForFunction((flag) => (window as never)[flag] === true, SETTLED_FLAG);
      await page.evaluate(() => document.fonts.ready);
    } catch (thrown) {
      waitFailure = thrown;
    }

    // `animations: "disabled"` stills the infinite pulses and sweeps on running
    // cards; `reducedMotion` stays at its default so the frame is the one that
    // ships. Never `toHaveScreenshot` — this is for looking at, not for diffing.
    await page.screenshot({ path: file, fullPage: true, animations: "disabled" });

    // "The page rendered", never "it looks right".
    const overlay = await page.locator("vite-error-overlay").count();
    expect(crashes, `${file}: the page raised an uncaught error`).toEqual([]);
    expect(overlay, `${file}: Vite's error overlay is showing`).toBe(0);
    if (waitFailure) throw waitFailure;
  });
}
