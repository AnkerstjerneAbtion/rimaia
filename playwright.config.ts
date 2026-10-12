import { defineConfig, devices } from "@playwright/test";

// Outside tsconfig.json's `include`, like vite.config.ts: type-checking it
// would need @types/node. Run it through `npm run screenshot`, which picks the
// port and the label.

// @ts-expect-error process is a nodejs global
const port = process.env.RIMAIA_SCREENSHOT_PORT;
if (!port) {
  throw new Error("RIMAIA_SCREENSHOT_PORT is not set — run `npm run screenshot` instead");
}

const HEIGHT = 900;

function project(colorScheme: "dark" | "light", width: number) {
  return {
    name: `${colorScheme}-${width}`,
    use: {
      ...devices["Desktop Safari"],
      colorScheme,
      viewport: { width, height: HEIGHT },
    },
  };
}

export default defineConfig({
  // Playwright collects every `*.{spec,test}.*` under the config's directory
  // by default, which is the whole vitest suite in `src/`.
  testDir: "screenshots",
  testMatch: "*.shot.ts",
  // Playwright empties this at the start of every run, so it is never a label.
  outputDir: ".screenshots/.playwright",
  reporter: "list",
  fullyParallel: true,
  retries: 0,
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    browserName: "webkit",
    locale: "en-US",
    timezoneId: "UTC",
  },
  projects: [
    project("dark", 1440),
    project("dark", 1024),
    project("light", 1440),
    project("light", 1024),
  ],
  webServer: {
    command: `npx vite --host 127.0.0.1 --port ${port} --strictPort`,
    url: `http://127.0.0.1:${port}/fixtures.html`,
    // Reusing a server already on this port would screenshot another
    // worktree's code and say nothing.
    reuseExistingServer: false,
    timeout: 60_000,
  },
});
