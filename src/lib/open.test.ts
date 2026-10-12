import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { installTransports } from "./client";
import { openExternalUrl } from "./open";

// `openUrl` reaches `invoke` itself, so the same mock the wrappers' tests use
// sees exactly what would cross the boundary.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("openExternalUrl", () => {
  it("sends the opener plugin's open_url with the url exactly", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await openExternalUrl("https://github.com/abtion/rimaia/pull/42");

    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("plugin:opener|open_url", {
      url: "https://github.com/abtion/rimaia/pull/42",
      with: undefined,
    });
  });

  it("rejects when the plugin refuses", async () => {
    mockInvoke.mockRejectedValue("url not allowed by the opener scope");

    await expect(openExternalUrl("https://blocked.example/")).rejects.toBe(
      "url not allowed by the opener scope",
    );
  });
});

describe("openExternalUrl in the browser", () => {
  const open = vi.fn();

  beforeEach(() => {
    open.mockReset();
    vi.stubGlobal("open", open);
    installTransports("browser");
  });

  afterEach(() => {
    installTransports("solo");
    vi.unstubAllGlobals();
  });

  it("opens a link in a new tab in browser mode", () => {
    // Not awaited: the tab must open in the keypress that asked for it.
    void openExternalUrl("https://github.com/o/r/pull/7");

    expect(open).toHaveBeenCalledTimes(1);
    expect(open).toHaveBeenCalledWith(
      "https://github.com/o/r/pull/7",
      "_blank",
      "noopener,noreferrer",
    );
    expect(mockInvoke).not.toHaveBeenCalled();
  });
});

describe("openExternalUrl refusing what is not a web link", () => {
  const open = vi.fn();
  const NOT_WEB_LINKS = [
    "javascript:alert(1)",
    "data:text/html,x",
    "file:///etc/passwd",
    "not a url",
  ];

  beforeEach(() => {
    open.mockReset();
    vi.stubGlobal("open", open);
  });

  afterEach(() => {
    installTransports("solo");
    vi.unstubAllGlobals();
  });

  it.each(["solo", "browser"] as const)("opens only web links in %s", async (mode) => {
    installTransports(mode);

    for (const url of NOT_WEB_LINKS) {
      await expect(openExternalUrl(url)).rejects.toEqual({
        code: "invalid",
        message: "Only web links can be opened.",
      });
    }

    expect(open).not.toHaveBeenCalled();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("opens http and https links", async () => {
    installTransports("browser");

    await openExternalUrl("http://example.test/a");
    await openExternalUrl("https://example.test/b");

    expect(open.mock.calls.map(([url]) => url)).toEqual([
      "http://example.test/a",
      "https://example.test/b",
    ]);
  });
});
