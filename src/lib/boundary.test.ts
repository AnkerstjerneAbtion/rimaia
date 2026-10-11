import { describe, expect, it } from "vitest";

/**
 * Where the transports are allowed to touch their libraries (task 049, D7).
 * "No component imports a Tauri plugin" stays true only if a second file that
 * reaches for one fails here, so the one allowed plugin import is named by
 * path, not by pattern.
 *
 * Reads every source file under `src/` except tests and `src/test/`, the
 * suite's own helpers, which import the very modules they replace so they can
 * configure the mocks.
 */
const modules = import.meta.glob<string>(
  ["/src/**/*.{ts,tsx}", "!/src/**/*.test.{ts,tsx}", "!/src/test/**"],
  { query: "?raw", import: "default", eager: true },
);

const FILES = Object.entries(modules).map(([path, text]) => ({
  path: path.replace(/^\/src\//, ""),
  text,
}));

/** The files whose source matches `pattern`. */
function importing(pattern: RegExp): string[] {
  return FILES.filter(({ text }) => pattern.test(text))
    .map(({ path }) => path)
    .sort();
}

describe("the transport boundary", () => {
  it("finds the source files it is meant to guard", () => {
    expect(FILES.length).toBeGreaterThan(50);
    expect(FILES.map(({ path }) => path)).toContain("lib/commands.ts");
    expect(FILES.map(({ path }) => path)).not.toContain("lib/commands.test.ts");
  });

  it("imports @tauri-apps only from commands.ts and events.ts, and a plugin only from open.ts", () => {
    expect(importing(/from\s+["']@tauri-apps\/api\/core["']/)).toEqual(["lib/commands.ts"]);
    expect(importing(/from\s+["']@tauri-apps\/api\/event["']/)).toEqual(["lib/events.ts"]);
    expect(importing(/from\s+["']@tauri-apps\/api\/(?!core["']|event["'])/)).toEqual([]);
    expect(importing(/from\s+["']@tauri-apps\/plugin-opener["']/)).toEqual(["lib/open.ts"]);
    expect(importing(/from\s+["']@tauri-apps\/plugin-/)).toEqual(["lib/open.ts"]);
  });

  it("imports fetch-event-source only from events.ts", () => {
    expect(importing(/from\s+["']@microsoft\/fetch-event-source["']/)).toEqual(["lib/events.ts"]);
  });

  it("installs transports only from main.tsx, the fixture entry and src/lib", () => {
    const setters = /\b(setCommandTransports?|setEventTransports?|setUrlOpener|bootstrapClient)\b/;
    const callers = FILES.filter(({ text }) => setters.test(text))
      .map(({ path }) => path)
      .filter((path) => !path.startsWith("lib/"));

    expect(callers.sort()).toEqual(["dev/main.tsx", "main.tsx"]);
  });
});
