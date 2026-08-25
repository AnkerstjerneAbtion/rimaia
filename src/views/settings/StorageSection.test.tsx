import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { StorageSection } from "./StorageSection";

// `commands.ts` is the only module that imports `invoke`; mocking it here
// exercises the real call path (including `toRimaiaError`) instead of
// stubbing the frontend's own command wrappers.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("StorageSection", () => {
  it("renders the data, database and logs paths once get_app_info resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_app_info") {
        return {
          appVersion: "0.1.0",
          dataDir: "/home/user/.local/share/rimaia",
          dbFile: "/home/user/.local/share/rimaia/rimaia.db",
          logsDir: "/home/user/.local/share/rimaia/logs",
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<StorageSection />);

    expect(await screen.findByText("/home/user/.local/share/rimaia")).toBeInTheDocument();
    expect(
      screen.getByText("/home/user/.local/share/rimaia/rimaia.db"),
    ).toBeInTheDocument();
    expect(screen.getByText("/home/user/.local/share/rimaia/logs")).toBeInTheDocument();
  });

  it("renders the error message inside the ErrorBanner when get_app_info rejects", async () => {
    mockInvoke.mockRejectedValue({ code: "io", message: "app data directory unreadable" });

    render(<StorageSection />);

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("app data directory unreadable");
  });
});
