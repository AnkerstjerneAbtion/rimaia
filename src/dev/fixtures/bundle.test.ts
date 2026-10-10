// @vitest-environment node
import { build } from "vite";
import type { Rollup } from "vite";
import { describe, expect, it } from "vitest";

import { FIXTURE_SENTINEL } from "./constants";

describe("the production bundle", () => {
  it(
    "keeps every fixture out of the production bundle",
    async () => {
      // vitest sets NODE_ENV=test, and Vite derives `import.meta.env.PROD` (and
      // React its production build) from NODE_ENV, so a "production" build under
      // vitest would otherwise be a test-mode one.
      // @ts-expect-error process is a nodejs global
      const env = process.env;
      const previous = env.NODE_ENV;
      env.NODE_ENV = "production";
      try {
        const result = await build({
          mode: "production",
          logLevel: "silent",
          build: { write: false },
        });
        const outputs = (Array.isArray(result) ? result : [result]) as Rollup.RollupOutput[];
        const chunks = outputs.flatMap((output) =>
          output.output.filter((item): item is Rollup.OutputChunk => item.type === "chunk"),
        );

        expect(chunks.length).toBeGreaterThan(0);
        expect(chunks.filter((chunk) => chunk.code.includes(FIXTURE_SENTINEL))).toEqual([]);
        const fixtureModules = chunks.flatMap((chunk) =>
          Object.keys(chunk.modules).filter((id) => id.includes("/src/dev/")),
        );
        expect(fixtureModules).toEqual([]);
      } finally {
        if (previous === undefined) delete env.NODE_ENV;
        else env.NODE_ENV = previous;
      }
    },
    120_000,
  );
});
