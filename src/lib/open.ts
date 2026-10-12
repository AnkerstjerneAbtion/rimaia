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
 * back to the board and no referrer.
 *
 * With `noopener` the call returns `null` whether or not a window opened, so a
 * blocked pop-up cannot be detected and is not reported. It is called from a
 * keypress, which browsers treat as a user gesture, so the blocker admits it:
 * {@link openExternalUrl} calls it before any `await` for that reason.
 */
export const browserUrlOpener: UrlOpener = async (url) => {
  globalThis.open(url, "_blank", "noopener,noreferrer");
};

/** What a link that is not a web link is refused with. The value is never
 *  echoed: it is text a run recorded from an agent's output. */
export const WEB_LINKS_ONLY = "Only web links can be opened.";

function isWebLink(url: string): boolean {
  try {
    const { protocol } = new URL(url);
    return protocol === "http:" || protocol === "https:";
  } catch {
    return false;
  }
}

/**
 * Opens a URL in the user's browser. The only module that imports
 * `@tauri-apps/plugin-opener`.
 *
 * **Only `http:` and `https:` links, in every mode** (task 050). A `pr_url` is
 * text a run recorded from an agent's output, and in a browser
 * `window.open("javascript:...")` would run it as script on the server's
 * origin, with the session cookie. The desktop gets the same check, so a link
 * the browser refuses is never one the desktop opens.
 *
 * `openUrl` sends `plugin:opener|open_url` through `invoke` directly, outside
 * `commands.ts`'s `CommandTransport`, which makes this a client-side
 * capability like the two Open in… commands: a browser has no such plugin and
 * installs {@link browserUrlOpener} instead (task 049). Keeping it behind one
 * function is what lets that swap happen in one place rather than in every
 * component that wants a link opened by a key.
 *
 * Not `async`: the opener is called in the caller's own turn, before any
 * `await`, because a browser lets a page open a tab only during the keypress
 * that asked for it.
 */
export function openExternalUrl(url: string): Promise<void> {
  if (!isWebLink(url)) {
    return Promise.reject({ code: "invalid", message: WEB_LINKS_ONLY } satisfies RimaiaError);
  }
  return (installedOpener ?? openUrl)(url);
}
