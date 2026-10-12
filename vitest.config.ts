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
        // Node would load the opener plugin as an external module, and its own
        // `import "@tauri-apps/api/core"` would then bypass `vi.mock`. Inlined,
        // `openUrl`'s `invoke` is the same mocked one every other test sees
        // (task 017: `openExternalUrl` is covered by that mock).
        server: { deps: { inline: ["@tauri-apps/plugin-opener"] } },
      },
    }),
  ),
);
