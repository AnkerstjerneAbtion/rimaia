import { afterEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { getAppInfo, listRepositories, setCommandTransport } from "./commands";

// Mocked at the Tauri seam, like `commands.test.ts`: the point of the default
// transport is that this mock keeps working with no edit anywhere else.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

afterEach(() => {
  setCommandTransport(undefined);
  mockInvoke.mockReset();
});

describe("the command transport", () => {
  it("sends every command through the installed transport, not invoke", async () => {
    const transport = vi.fn().mockResolvedValue([]);
    setCommandTransport(transport);

    await expect(listRepositories()).resolves.toEqual([]);

    expect(transport).toHaveBeenCalledWith("list_repositories", undefined);
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("answers through Tauri's invoke when no transport is installed", async () => {
    mockInvoke.mockResolvedValue({ appVersion: "1.2.3" });

    await expect(getAppInfo()).resolves.toEqual({ appVersion: "1.2.3" });

    expect(mockInvoke).toHaveBeenCalledWith("get_app_info", undefined);
  });

  it("normalises a transport's rejection into a RimaiaError", async () => {
    setCommandTransport(() => Promise.reject("a bare string"));
    await expect(getAppInfo()).rejects.toEqual({ code: "internal", message: "a bare string" });

    setCommandTransport(() => Promise.reject(new Error("boom")));
    await expect(getAppInfo()).rejects.toEqual({ code: "internal", message: "boom" });
  });

  it("passes a transport's own {code, message} refusal through unchanged", async () => {
    setCommandTransport(() => Promise.reject({ code: "invalid", message: "nope" }));

    await expect(getAppInfo()).rejects.toEqual({ code: "invalid", message: "nope" });
  });
});
