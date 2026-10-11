import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke, isTauri } from "@tauri-apps/api/core";

import { bootstrapClient, csrfFromCookie, installTransports } from "./client";
import { listRepositories, setCommandTransports } from "./commands";
import { setEventTransports } from "./events";
import { openExternalUrl, setUrlOpener } from "./open";
import { mockHttp } from "../test/http";
import type { ClientCapabilities } from "../types";

// `runningInDesktopShell()` is driven through this mock, which provides
// `isTauri` beside `invoke`.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockIsTauri = vi.mocked(isTauri);

function desktop(mode: "solo" | "connected"): ClientCapabilities {
  return {
    mode,
    localRunnerId: "runner-1",
    canOpenInEditor: true,
    canRevealFiles: true,
    canChooseFolders: true,
  };
}

const PULL_REQUEST = "https://github.com/o/r/pull/1";

beforeEach(() => {
  mockInvoke.mockReset();
  mockIsTauri.mockReset();
});

afterEach(() => {
  setCommandTransports(undefined);
  setEventTransports(undefined);
  setUrlOpener(undefined);
  vi.unstubAllGlobals();
  document.cookie = "rimaia_csrf=; expires=Thu, 01 Jan 1970 00:00:00 GMT";
});

describe("bootstrapClient", () => {
  it("installs HTTP and SSE with a session credential at the page's origin in the browser", async () => {
    const http = mockHttp();
    http.answer("list_repositories", []);
    mockIsTauri.mockReturnValue(false);

    await bootstrapClient();
    await listRepositories();

    expect(http.requests).toHaveLength(1);
    expect(http.requests[0].url).toBe(`${window.location.origin}/api/v1/list_repositories`);
    expect(http.requests[0].credentials).toBe("same-origin");
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("leaves invoke and listen installed in solo", async () => {
    const http = mockHttp();
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (command) =>
      command === "get_client_capabilities" ? desktop("solo") : [],
    );

    await bootstrapClient();
    await listRepositories();

    expect(mockInvoke.mock.calls.map(([command]) => command)).toEqual([
      "get_client_capabilities",
      "list_repositories",
    ]);
    expect(http.requests).toEqual([]);
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });

  it("fails loudly naming task 059 when the shell answers connected", async () => {
    mockHttp();
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (command) =>
      command === "get_client_capabilities" ? desktop("connected") : [],
    );

    await expect(bootstrapClient()).rejects.toEqual({
      code: "internal",
      message:
        "This desktop is set to connect to a server, which this build cannot do yet (task 059).",
    });

    // Nothing was installed: a board command still goes to invoke.
    await listRepositories();
    expect(mockInvoke).toHaveBeenLastCalledWith("list_repositories", undefined);
    expect(vi.mocked(fetch)).not.toHaveBeenCalled();
  });
});

describe("opening a link", () => {
  it("opens a pull request with window.open in the browser", async () => {
    const open = vi.fn();
    vi.stubGlobal("open", open);
    installTransports("browser");

    await openExternalUrl(PULL_REQUEST);

    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith(PULL_REQUEST, "_blank", "noopener,noreferrer");
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("refuses a link that is not http or https in the browser", async () => {
    const open = vi.fn();
    vi.stubGlobal("open", open);
    installTransports("browser");

    await expect(openExternalUrl("javascript:alert(1)")).rejects.toEqual({
      code: "invalid",
      message: "Only http and https links can be opened: javascript:alert(1).",
    });

    expect(open).not.toHaveBeenCalled();
  });

  it("still opens a pull request through the opener plugin in solo", async () => {
    const open = vi.fn();
    vi.stubGlobal("open", open);
    mockIsTauri.mockReturnValue(true);
    mockInvoke.mockImplementation(async (command) =>
      command === "get_client_capabilities" ? desktop("solo") : undefined,
    );
    await bootstrapClient();

    await openExternalUrl(PULL_REQUEST);

    expect(mockInvoke).toHaveBeenLastCalledWith("plugin:opener|open_url", {
      url: PULL_REQUEST,
      with: undefined,
    });
    expect(open).not.toHaveBeenCalled();
  });
});

describe("csrfFromCookie", () => {
  it("returns the rimaia_csrf cookie as read, and null without one", () => {
    expect(csrfFromCookie()).toBeNull();

    document.cookie = "other=1";
    document.cookie = "rimaia_csrf=abc-DEF_123";
    expect(csrfFromCookie()).toBe("abc-DEF_123");
  });
});
