import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";

import { RepositoriesSection } from "./RepositoriesSection";
import type { Repository } from "../../types";

// `commands.ts` and `events.ts` both bottom out in `@tauri-apps/api/core`'s
// `invoke` - mocking here, rather than the wrappers, exercises the real
// `toRimaiaError` path exactly as `StorageSection.test.tsx` does.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

// `@tauri-apps/api/event`'s real `listen` rejects under jsdom (there is no
// Tauri IPC bridge to call `transformCallback` on), which every other test
// below relies on: the component's own `.then(success, () => {/* no live
// refresh */})` swallows that rejection silently. Mocked here so the one
// test below that cares about the live-refresh path can drive it directly.
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

// `@tauri-apps/plugin-dialog` is a separate node_modules package that
// Vitest's dep graph does not route through the `@tauri-apps/api/core` mock
// above (its own `invoke` import resolves outside the transformed module
// graph) - it has no `toRimaiaError` logic of its own to exercise, so it's
// mocked directly at its own boundary instead.
vi.mock("@tauri-apps/plugin-dialog", () => ({
  open: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);
const mockOpen = vi.mocked(open);

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/Users/dev/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/Users/dev/.local/share/rimaia/worktrees/rimaia",
    allowUnattendedRuns: false,
    createdAt: "2026-08-20T12:00:00+00:00",
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockOpen.mockReset();
  mockListen.mockReset();
  // The default every test but the live-refresh one below wants: a
  // subscription that resolves (so the component's success branch runs) and
  // never fires.
  mockListen.mockResolvedValue(vi.fn());
});

describe("RepositoriesSection", () => {
  it("renders every registered repository with its default branch and remote", async () => {
    const repo = repository();
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") {
        return { remoteUrl: "git@github.com:example/rimaia.git", ghReady: true };
      }
      throw new Error(`unexpected command: ${command} ${JSON.stringify(args)}`);
    });

    render(<RepositoriesSection />);

    expect(await screen.findByText("rimaia")).toBeInTheDocument();
    expect(screen.getByText("/Users/dev/code/rimaia")).toBeInTheDocument();
    expect(screen.getByText("main")).toBeInTheDocument();
    expect(
      await screen.findByText("git@github.com:example/rimaia.git"),
    ).toBeInTheDocument();
  });

  it("shows the specific backend message for one invalid path, not a generic failure", async () => {
    mockOpen.mockResolvedValue("/tmp/not-a-repo");
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return [];
      if (command === "register_repository") {
        throw { code: "invalid", message: "/tmp/not-a-repo is not a git repository" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    fireEvent.click(screen.getByRole("button", { name: "Add repository" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("/tmp/not-a-repo is not a git repository");
  });

  it("shows a different specific backend message for a repository with no commits", async () => {
    mockOpen.mockResolvedValue("/tmp/empty-repo");
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return [];
      if (command === "register_repository") {
        throw { code: "invalid", message: "/tmp/empty-repo has no commits yet" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    fireEvent.click(screen.getByRole("button", { name: "Add repository" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("/tmp/empty-repo has no commits yet");
  });

  it("defaults the unattended opt-in to off and requires confirming the honest warning before enabling it", async () => {
    const repo = repository({ allowUnattendedRuns: false });
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_unattended_runs") {
        throw new Error("set_repository_unattended_runs must not be called before confirmation");
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const checkbox = await screen.findByRole("checkbox", {
      name: "Allow unattended agent runs",
    });
    expect(checkbox).not.toBeChecked();

    fireEvent.click(checkbox);

    // The confirmation states the permission plainly, unhedged.
    const confirmDialog = await screen.findByRole("alertdialog");
    expect(confirmDialog).toHaveTextContent(
      "the agent can run any command in this repository's worktree, including network access and package installation, without asking.",
    );

    // Cancelling never calls the backend, and the checkbox stays off.
    fireEvent.click(within(confirmDialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(checkbox).not.toBeChecked();
  });

  it("enables unattended runs only once the confirmation is accepted", async () => {
    const repo = repository({ allowUnattendedRuns: false });
    let allow = false;
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_repositories") return [{ ...repo, allowUnattendedRuns: allow }];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "set_repository_unattended_runs") {
        allow = (args as { allow: boolean }).allow;
        return { ...repo, allowUnattendedRuns: allow };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    const checkbox = await screen.findByRole("checkbox", {
      name: "Allow unattended agent runs",
    });
    fireEvent.click(checkbox);

    const confirmDialog = await screen.findByRole("alertdialog");
    fireEvent.click(
      within(confirmDialog).getByRole("button", { name: "Enable unattended runs" }),
    );

    await waitFor(() => {
      expect(screen.getByRole("checkbox", { name: "Allow unattended agent runs" })).toBeChecked();
    });
  });

  it("shows the backend's refusal, including the task count, when removing a repository with tasks", async () => {
    const repo = repository();
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") return [repo];
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      if (command === "remove_repository") {
        throw { code: "invalid", message: "cannot remove this repository: 3 tasks still reference it" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositoriesSection />);

    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));

    const banner = await screen.findByRole("alert");
    expect(banner).toHaveTextContent("cannot remove this repository: 3 tasks still reference it");
  });

  it("live-refreshes the list when the backend publishes repositories:changed", async () => {
    const repo = repository();
    let listCalls = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_repositories") {
        listCalls += 1;
        return listCalls === 1 ? [] : [repo];
      }
      if (command === "get_repository_remote_info") return { remoteUrl: null, ghReady: null };
      throw new Error(`unexpected command: ${command}`);
    });
    let handler: ((event: { payload: string[] }) => void) | undefined;
    mockListen.mockImplementation(async (_eventName, callback) => {
      handler = callback as (event: { payload: string[] }) => void;
      return vi.fn();
    });

    render(<RepositoriesSection />);
    await screen.findByText("No repositories registered yet.");

    // The literal event name the shell forwarder publishes on (ADR-0018,
    // seam-contract D7) - renaming either side keeps the rest of the gate
    // green while live refresh silently stops, which is exactly what this
    // test exists to catch.
    expect(mockListen).toHaveBeenCalledWith("repositories:changed", expect.any(Function));

    // The empty-array payload is the shell forwarder's own "re-read this
    // entity wholesale" signal (see `events.ts`'s own contract note), so the
    // handler takes no ids and just refreshes.
    handler?.({ payload: [] });

    expect(await screen.findByText("rimaia")).toBeInTheDocument();
    expect(listCalls).toBe(2);
  });
});
