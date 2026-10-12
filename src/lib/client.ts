import { reportSignedOut, reportStaleBundle } from "./auth";
import {
  PROTOCOL_VERSION,
  browserLocalTransport,
  getClientCapabilities,
  httpCommandTransport,
  invokeCommandTransport,
  runningInDesktopShell,
  setCommandTransports,
} from "./commands";
import type { Credential } from "./commands";
import {
  listenEventTransport,
  setEventTransports,
  sseEventTransport,
  subscribeToEventStreamFailure,
} from "./events";
import { browserUrlOpener, setUrlOpener } from "./open";
import type { ClientCapabilities, RimaiaError } from "../types";

/**
 * Where a connected desktop's board lives and how it proves who it is
 * (ADR-0030 point 4). Nothing in task 049 supplies one: task 059 adds the
 * local command that does.
 */
export interface ServerConnection {
  serverUrl: string;
  token: string;
}

/** The CSRF cookie a server sets for a browser session (task 047 Scope 11). */
const CSRF_COOKIE = "rimaia_csrf";

/**
 * The CSRF value, read from the readable `rimaia_csrf` cookie on every call,
 * or `null` when there is none. The session cookie itself is `HttpOnly`
 * (ADR-0030 point 2) and never read here. The value is base64url, so it is
 * sent back as read, with no decoding.
 */
export function csrfFromCookie(): string | null {
  for (const pair of document.cookie.split(";")) {
    const separator = pair.indexOf("=");
    if (separator === -1) continue;
    if (pair.slice(0, separator).trim() !== CSRF_COOKIE) continue;
    const value = pair.slice(separator + 1).trim();
    return value === "" ? null : value;
  }
  return null;
}

/** Where the page remembers which bundle it has already reloaded into. */
export const UPGRADE_RELOAD_KEY = "rimaia.upgradeReload";

/**
 * The server is newer than this page: reload, once per bundle (D32 point 7).
 *
 * `sessionStorage` holds the `PROTOCOL_VERSION` literal of the bundle that
 * last reloaded. Nothing stored, or a different value, means this bundle has
 * not reloaded yet, so it stores its own and reloads. An equal value means
 * this bundle was already reloaded into and is still too old, so it says so
 * and does not reload again. Nothing clears the value: a read answered under
 * skew says nothing about the bundle, and a bundle that fixes the skew carries
 * a different literal.
 */
function reloadOncePerBundle(): void {
  try {
    if (globalThis.sessionStorage.getItem(UPGRADE_RELOAD_KEY) === PROTOCOL_VERSION) {
      reportStaleBundle();
      return;
    }
    globalThis.sessionStorage.setItem(UPGRADE_RELOAD_KEY, PROTOCOL_VERSION);
  } catch {
    // No storage means no memory of having reloaded: reloading could loop.
    reportStaleBundle();
    return;
  }
  globalThis.location.reload();
}

/** Stops listening to the event stream's failures, in a browser. */
let stopWatchingStream: (() => void) | null = null;

/**
 * Turns the stream's fatal failures into what the page reacts to: a `401` is
 * the end of the session and a `426` is the reload above. Installed in a
 * browser only, and removed when another mode is installed.
 */
function watchStream(): void {
  stopWatchingStream?.();
  stopWatchingStream = subscribeToEventStreamFailure(({ error }) => {
    if (error.code === "unauthenticated") reportSignedOut();
    if (error.code === "upgrade_required") reloadOncePerBundle();
  });
}

/**
 * Installs the transports a mode needs (ADR-0034 point 4); pure apart from
 * calling the setters.
 *
 * - `solo` leaves the defaults: `invoke` and `listen` for everything.
 * - `browser`: HTTP and SSE at the page's origin with a session credential,
 *   every local command refused, no local events, `window.open` for links.
 * - `connected`: HTTP and SSE with a bearer token for the board, `invoke` and
 *   `listen` for this machine.
 */
export function installTransports(
  mode: "solo" | "browser" | "connected",
  connection?: ServerConnection,
): void {
  stopWatchingStream?.();
  stopWatchingStream = null;
  if (mode === "solo") {
    setCommandTransports(undefined);
    setEventTransports(undefined);
    setUrlOpener(undefined);
    return;
  }
  if (mode === "browser") {
    const server = {
      baseUrl: window.location.origin,
      credential: { kind: "session", csrf: csrfFromCookie } satisfies Credential,
    };
    setCommandTransports({
      board: httpCommandTransport({ ...server, onUpgradeRequired: reloadOncePerBundle }),
      local: browserLocalTransport,
    });
    setEventTransports({ board: sseEventTransport(server), local: null });
    setUrlOpener(browserUrlOpener);
    watchStream();
    return;
  }
  if (!connection) {
    throw new Error("a connected desktop needs a server URL and a token");
  }
  const server = {
    baseUrl: connection.serverUrl,
    credential: { kind: "bearer", token: connection.token } satisfies Credential,
  };
  setCommandTransports({
    board: httpCommandTransport(server),
    local: invokeCommandTransport,
  });
  setEventTransports({ board: sseEventTransport(server), local: listenEventTransport });
  setUrlOpener(undefined);
}

/**
 * Chooses the transports for wherever this page is running; `main.tsx` awaits
 * it before the first render. The fixture entry never calls it.
 *
 * Resolves with the answer it installed transports for, which `main.tsx`
 * hands to the capabilities provider so components read it and never ask.
 *
 * A desktop asks its shell what it is. A shell that answers `connected` is
 * refused loudly until task 059 supplies the server URL and token, rather
 * than falling back to solo and writing to a board nobody reads (D32 point 3).
 */
export async function bootstrapClient(): Promise<ClientCapabilities> {
  if (!runningInDesktopShell()) {
    installTransports("browser");
    // The browser answers it itself, without a request.
    return getClientCapabilities();
  }
  const capabilities = await getClientCapabilities();
  if (capabilities.mode === "connected") {
    throw {
      code: "internal",
      message:
        "This desktop is set to connect to a server, which this build cannot do yet (task 059).",
    } satisfies RimaiaError;
  }
  installTransports("solo");
  return capabilities;
}
