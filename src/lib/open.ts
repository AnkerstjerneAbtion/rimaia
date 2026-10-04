import { openUrl } from "@tauri-apps/plugin-opener";

/**
 * Opens a URL in the user's browser. The only module that imports
 * `@tauri-apps/plugin-opener`.
 *
 * `openUrl` sends `plugin:opener|open_url` through `invoke` directly, outside
 * `commands.ts`'s `CommandTransport`, which makes this a client-side
 * capability like the two Open in… commands: the browser client has no such
 * plugin. Keeping it behind one function is what lets task 049 swap it for
 * `window.open` in one place rather than in every component that wants a link
 * opened by a key.
 */
export function openExternalUrl(url: string): Promise<void> {
  return openUrl(url);
}
