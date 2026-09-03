import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { WorktreeSection } from "./WorktreeSection";
import type { WorktreeStatus } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` or `lib/events.ts` — see
// `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function worktreeStatus(overrides: Partial<WorktreeStatus> = {}): WorktreeStatus {
  return {
    taskId: "task-1",
    exists: true,
    path: "/data/worktrees/repo/task-1",
    branch: "rimaia/task-1-wire-the-board",
    baseRef: "main",
    dependencyWarning: null,
    ahead: 2,
    behind: 1,
    dirty: true,
    commitCount: 2,
    diff: { filesChanged: 3, insertions: 10, deletions: 4 },
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("WorktreeSection", () => {
  it("shows ADR-0008's multi-dependency warning verbatim, before the task has ever run", async () => {
    // The warning is about what the *next* run will be built on, so it has to
    // appear while the user can still act on it — which is exactly the state
    // where there is no worktree yet.
    const warning =
      'This task branches from "Add the API endpoint" (rimaia/task-0-add-the-api-endpoint). ' +
      '"Add the schema" is also a dependency and is not in that base — merge into it what ' +
      "you need, or run this task again once the rest have landed.";
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") {
        return worktreeStatus({
          exists: false,
          path: null,
          branch: null,
          dependencyWarning: warning,
        });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    expect(await screen.findByText(warning)).toBeInTheDocument();
  });

  it("says nothing when there is nothing to warn about", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    await screen.findByText("rimaia/task-1-wire-the-board");
    expect(screen.queryByText(/is also a dependency/)).not.toBeInTheDocument();
  });

  it("renders the deliberate empty case for a task that has never run", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") {
        return worktreeStatus({ exists: false, path: null, branch: null, ahead: 0, behind: 0, dirty: false, commitCount: 0, diff: { filesChanged: 0, insertions: 0, deletions: 0 } });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    expect(await screen.findByText(/has never run/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open in Finder/Explorer" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy path" })).not.toBeInTheDocument();
  });

  it("renders branch, path and live status once get_worktree_status resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    expect(await screen.findByText("rimaia/task-1-wire-the-board")).toBeInTheDocument();
    expect(screen.getByText("/data/worktrees/repo/task-1")).toBeInTheDocument();
    expect(screen.getByText(/Uncommitted changes/)).toBeInTheDocument();
    expect(screen.getByText(/2 ahead \/ 1 behind main/)).toBeInTheDocument();
    expect(screen.getByText(/3 files changed \(\+10 \/ -4\)/)).toBeInTheDocument();
  });

  it("renders the missing-on-disk case and disables Open when the recorded path no longer resolves", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus({ exists: false });
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    expect(await screen.findByText(/Missing on disk/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open in Finder/Explorer" })).toBeDisabled();
    // "Copy path" still works — the path string is harmless to copy even
    // when nothing resolves it on disk.
    expect(screen.getByRole("button", { name: "Copy path" })).toBeEnabled();
  });

  it("calls reveal_task_worktree when Open in Finder/Explorer is clicked", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "reveal_task_worktree") return undefined;
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Open in Finder/Explorer" }));

    expect(mockInvoke).toHaveBeenCalledWith("reveal_task_worktree", { taskId: "task-1" });
    // Waits for the button's own "Opening…" state to resolve so the
    // `setRevealing(false)` this click eventually causes lands inside the
    // test, not after it as an unwrapped `act` warning.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Open in Finder/Explorer" })).toBeEnabled(),
    );
  });

  it("shows the error banner when reveal_task_worktree rejects", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "reveal_task_worktree") {
        throw { code: "internal", message: "could not open the worktree directory" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Open in Finder/Explorer" }));

    expect(
      await screen.findByText("could not open the worktree directory"),
    ).toBeInTheDocument();
  });

  it("copies the worktree path to the clipboard without a backend round trip", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy path" }));

    await waitFor(() => expect(writeText).toHaveBeenCalledWith("/data/worktrees/repo/task-1"));
    expect(mockInvoke).not.toHaveBeenCalledWith("copy_task_worktree_path", expect.anything());
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("shows an error when the clipboard write itself fails", async () => {
    const writeText = vi.fn().mockRejectedValue(new Error("denied"));
    Object.assign(navigator, { clipboard: { writeText } });

    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") return worktreeStatus();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);
    fireEvent.click(await screen.findByRole("button", { name: "Copy path" }));

    expect(
      await screen.findByText("could not copy the path to the clipboard"),
    ).toBeInTheDocument();
  });

  it("refetches status when tasks:changed names this task's id", async () => {
    let resolveCount = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") {
        resolveCount += 1;
        return worktreeStatus({ ahead: resolveCount === 1 ? 0 : 5 });
      }
      throw new Error(`unexpected command: ${command}`);
    });

    let changedHandler: ((ids: string[]) => void) | undefined;
    mockListen.mockImplementation(async (_name, callback) => {
      changedHandler = (ids: string[]) => callback({ payload: ids } as never);
      return vi.fn();
    });

    render(<WorktreeSection taskId="task-1" />);
    expect(await screen.findByText(/0 ahead/)).toBeInTheDocument();

    changedHandler?.(["task-1"]);

    expect(await screen.findByText(/5 ahead/)).toBeInTheDocument();
  });

  it("does not refetch when tasks:changed names a different task's id", async () => {
    let calls = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") {
        calls += 1;
        return worktreeStatus();
      }
      throw new Error(`unexpected command: ${command}`);
    });

    let changedHandler: ((ids: string[]) => void) | undefined;
    mockListen.mockImplementation(async (_name, callback) => {
      changedHandler = (ids: string[]) => callback({ payload: ids } as never);
      return vi.fn();
    });

    render(<WorktreeSection taskId="task-1" />);
    await screen.findByText("rimaia/task-1-wire-the-board");
    expect(calls).toBe(1);

    changedHandler?.(["some-other-task"]);

    await Promise.resolve();
    expect(calls).toBe(1);
  });

  it("shows the error banner when get_worktree_status rejects", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_worktree_status") {
        throw { code: "internal", message: "git worktree list failed" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<WorktreeSection taskId="task-1" />);

    expect(await screen.findByText("git worktree list failed")).toBeInTheDocument();
  });
});
