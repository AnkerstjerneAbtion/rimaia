#!/usr/bin/env node
//
// `npm run screenshot` — render the UI in fixture mode and write PNGs to
// `.screenshots/<label>/` so a run (or a person) can look at what it changed.
//
//   npm run screenshot                       # all views -> .screenshots/latest/
//   npm run screenshot -- --label before     # -> .screenshots/before/
//   npm run screenshot -- --grep runs        # only matching captures
//
// Everything else on the command line is handed to `playwright test`.
//
// Two things this does that `playwright test` alone would not:
//   * It asks the operating system for a free port. 1420 belongs to
//     `npm run tauri dev` (strictPort), and only one worktree at a time can
//     hold it; two worktrees running this at once must both succeed.
//   * It clears the label's directory, but only for an un-narrowed run: a
//     `--grep` run overwrites the files it writes and leaves the rest, so
//     retaking one view does not delete the other 43.
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function parseArguments(argv) {
  let label = "latest";
  const rest = [];
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--label") {
      label = argv[index + 1];
      index += 1;
    } else if (argument.startsWith("--label=")) {
      label = argument.slice("--label=".length);
    } else {
      rest.push(argument);
    }
  }
  return { label, rest };
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

const { label, rest } = parseArguments(process.argv.slice(2));

// A label is a directory name under `.screenshots/`. One beginning with `.`
// could be `.playwright`, the directory Playwright empties at every run.
if (!label || label.startsWith(".") || /[\\/]/.test(label)) {
  console.error(
    `screenshot: refusing label ${JSON.stringify(label)} — use a plain name that does not begin with "."`,
  );
  process.exit(2);
}

const port = await freePort();

if (rest.length === 0) {
  rmSync(join(root, ".screenshots", label), { recursive: true, force: true });
}

const child = spawn("npx", ["playwright", "test", ...rest], {
  cwd: root,
  stdio: "inherit",
  env: {
    ...process.env,
    RIMAIA_SCREENSHOT_PORT: String(port),
    RIMAIA_SCREENSHOT_LABEL: label,
  },
});

child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 1);
});
