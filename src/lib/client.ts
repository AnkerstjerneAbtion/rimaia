import {
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
} from "./events";
import { browserUrlOpener, setUrlOpener } from "./open";
import type { RimaiaError } from "../types";

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
      board: httpCommandTransport(server),
      local: browserLocalTransport,
    });
    setEventTransports({ board: sseEventTransport(server), local: null });
    setUrlOpener(browserUrlOpener);
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
 * A desktop asks its shell what it is. A shell that answers `connected` is
 * refused loudly until task 059 supplies the server URL and token, rather
 * than falling back to solo and writing to a board nobody reads (D32 point 3).
 */
export async function bootstrapClient(): Promise<void> {
  if (!runningInDesktopShell()) {
    installTransports("browser");
    return;
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
}
