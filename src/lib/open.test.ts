import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

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

    await expect(openExternalUrl("javascript:alert(1)")).rejects.toBe(
      "url not allowed by the opener scope",
    );
  });
});
