import { openUrl } from "@tauri-apps/plugin-opener";

import type { RimaiaError } from "../types";

/** Opens a URL in the user's browser. */
export type UrlOpener = (url: string) => Promise<void>;

// `undefined` means the opener plugin's `openUrl`, looked up at call time so a
// test's `invoke` mock is the one it reaches, as `commands.ts` does for `invoke`.
let installedOpener: UrlOpener | undefined;

/**
 * Replace the opener. An entry point calls this (`installTransports` in
 * `client.ts`), a component never does. Beside the transport setters and on
 * the same terms: with nothing installed the desktop behaves as it always has.
 */
export function setUrlOpener(opener: UrlOpener | undefined): void {
  installedOpener = opener;
}

/**
 * The browser's opener: `window.open` in a new tab, with no `opener` handle
 * back to the board and no referrer. Only `http` and `https`: a `pr_url` is
 * written by a run, and the page that would open it is the board, holding a
 * session, so `javascript:` and friends are refused.
 *
 * With `noopener` the call returns `null` whether or not a window opened, so a
 * blocked pop-up cannot be detected and is not reported. It is called from a
 * keypress, which browsers treat as a user gesture, so the blocker admits it.
 */
export const browserUrlOpener: UrlOpener = async (url) => {
  let protocol: string | null = null;
  try {
    protocol = new URL(url).protocol;
  } catch {
    // Not a URL at all: refused below, like any other scheme.
  }
  if (protocol !== "http:" && protocol !== "https:") {
    throw {
      code: "invalid",
      message: `Only http and https links can be opened: ${url}.`,
    } satisfies RimaiaError;
  }
  globalThis.open(url, "_blank", "noopener,noreferrer");
};

/**
 * Opens a URL in the user's browser. The only module that imports
 * `@tauri-apps/plugin-opener`.
 *
 * `openUrl` sends `plugin:opener|open_url` through `invoke` directly, outside
 * `commands.ts`'s `CommandTransport`, which makes this a client-side
 * capability like the two Open in… commands: a browser has no such plugin and
 * installs {@link browserUrlOpener} instead (task 049). Keeping it behind one
 * function is what lets that swap happen in one place rather than in every
 * component that wants a link opened by a key.
 */
export function openExternalUrl(url: string): Promise<void> {
  return (installedOpener ?? openUrl)(url);
}
