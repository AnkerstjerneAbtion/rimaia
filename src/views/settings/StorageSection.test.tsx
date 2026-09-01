import { fireEvent, render, screen } from "@testing-library/react";
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

const APP_INFO = {
  appVersion: "0.1.0",
  dataDir: "/home/user/.local/share/rimaia",
  dbFile: "/home/user/.local/share/rimaia/rimaia.db",
  logsDir: "/home/user/.local/share/rimaia/logs",
};

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("StorageSection", () => {
  it("renders the data, database and logs paths once get_app_info resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 0;
      throw new Error(`unexpected command: ${String(command)}`);
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

  it("renders the total run-log size once get_run_log_size resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 1536;
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);

    expect(await screen.findByText("1.5 KB")).toBeInTheDocument();
  });

  it("prunes logs older than the chosen preset and refreshes the reported size", async () => {
    let sizeAfterPrune = 5_000_000;
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return sizeAfterPrune;
      if (command === "prune_run_logs") {
        expect(args).toEqual({ criterion: { kind: "older_than_days", days: 30 } });
        sizeAfterPrune = 1_000_000;
        return { runsPruned: 3, bytesFreed: 4_000_000 };
      }
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    await screen.findByText("4.8 MB");

    fireEvent.click(screen.getByRole("button", { name: "Older than 30 days" }));
    fireEvent.click(await screen.findByRole("button", { name: "Delete transcripts" }));

    expect(
      await screen.findByText("Removed 3 logs, freed 3.8 MB."),
    ).toBeInTheDocument();
    // The reported size refreshes to what `get_run_log_size` now answers,
    // not merely to "previous minus bytesFreed" computed client-side.
    expect(await screen.findByText("976.6 KB")).toBeInTheDocument();
  });

  // Picking the preset only *proposes* the deletion — this one spans every
  // task on the board, so the confirm gate matters more here than anywhere.
  it("prunes nothing when the confirmation is cancelled", async () => {
    const commands: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      commands.push(String(command));
      if (command === "get_app_info") return APP_INFO;
      if (command === "get_run_log_size") return 5_000_000;
      throw new Error(`unexpected command: ${String(command)}`);
    });

    render(<StorageSection />);
    await screen.findByText("4.8 MB");

    fireEvent.click(screen.getByRole("button", { name: "Older than 30 days" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(commands).not.toContain("prune_run_logs");
    expect(screen.getByRole("button", { name: "Older than 30 days" })).toBeInTheDocument();
  });
});
