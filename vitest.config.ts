import { defineConfig, mergeConfig } from "vitest/config";

import viteConfig from "./vite.config";

// A separate file, not a `test` key on the Vite config: `vite.config.ts`'s
// `defineConfig` is async and its return type has no `test` field.
// `mergeConfig` keeps the React plugin (and the dev-server tweaks) shared
// instead of duplicated.
export default defineConfig(async (env) =>
  mergeConfig(
    await viteConfig(env),
    defineConfig({
      test: {
        environment: "jsdom",
        globals: true,
        setupFiles: ["src/test/setup.ts"],
      },
    }),
  ),
);
