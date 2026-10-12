import { StrictMode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { useCheckouts } from "./useCheckouts";
import { useLocalWorktrees } from "./useLocalWorktrees";
import type { CheckoutView } from "../types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function checkout(overrides: Partial<CheckoutView> = {}): CheckoutView {
  return {
    repositoryId: "repo-1",
    path: "/code/rimaia",
    worktreeRoot: "/data/worktrees/rimaia",
    maxConcurrency: 1,
    unattendedConsent: true,
    onArchive: "none",
    onArchiveScript: null,
    ...overrides,
  };
}

/** Every event listener registered, by event name, so a test can fire one. */
let listeners: Record<string, Array<(event: { payload: unknown }) => void>> = {};

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  listeners = {};
  mockListen.mockImplementation(async (name, callback) => {
    (listeners[name as string] ??= []).push(callback as (event: { payload: unknown }) => void);
    return vi.fn();
  });
});

describe("useCheckouts", () => {
  it("keys this computer's checkouts by repository id, and is loading until they arrive", async () => {
    mockInvoke.mockResolvedValue([checkout()]);

    const { result } = renderHook(() => useCheckouts());

    expect(result.current.loading).toBe(true);
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.checkouts.get("repo-1")?.path).toBe("/code/rimaia");
    // A repository with no entry is not set up on this computer.
    expect(result.current.checkouts.has("repo-2")).toBe(false);
  });

  it("shares one read across every component mounted at once", async () => {
    mockInvoke.mockResolvedValue([checkout()]);

    const first = renderHook(() => useCheckouts());
    const second = renderHook(() => useCheckouts());
    await waitFor(() => expect(second.result.current.loading).toBe(false));
    expect(first.result.current.checkouts.size).toBe(1);

    expect(mockInvoke.mock.calls.filter(([name]) => name === "list_checkouts")).toHaveLength(1);
  });

  it("re-reads on repositories:changed, which every checkout write publishes", async () => {
    mockInvoke.mockResolvedValueOnce([checkout({ unattendedConsent: false })]);
    const { result } = renderHook(() => useCheckouts());
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.checkouts.get("repo-1")?.unattendedConsent).toBe(false);

    mockInvoke.mockResolvedValueOnce([checkout({ unattendedConsent: true })]);
    await waitFor(() => expect(listeners["repositories:changed"]).toBeDefined());
    act(() => {
      for (const listener of listeners["repositories:changed"] ?? []) listener({ payload: [] });
    });

    await waitFor(() =>
      expect(result.current.checkouts.get("repo-1")?.unattendedConsent).toBe(true),
    );
  });

  it("still answers under StrictMode's mount, unmount and mount again", async () => {
    mockInvoke.mockResolvedValue([checkout()]);

    const { result } = renderHook(() => useCheckouts(), { wrapper: StrictMode });

    await waitFor(() => expect(result.current.checkouts.size).toBe(1));
  });

  it("reports a failed read rather than claiming nothing is set up", async () => {
    mockInvoke.mockRejectedValue({ code: "internal", message: "the runner store is locked" });

    const { result } = renderHook(() => useCheckouts());

    await waitFor(() => expect(result.current.error?.message).toBe("the runner store is locked"));
    expect(result.current.loading).toBe(false);
  });
});

describe("useLocalWorktrees", () => {
  it("keys this computer's worktree records by task id, and re-reads on tasks:changed", async () => {
    mockInvoke.mockResolvedValueOnce([{ taskId: "task-1", path: "/data/worktrees/task-1" }]);
    const { result } = renderHook(() => useLocalWorktrees());
    await waitFor(() => expect(result.current.worktrees.get("task-1")).toBe("/data/worktrees/task-1"));

    mockInvoke.mockResolvedValueOnce([]);
    await waitFor(() => expect(listeners["tasks:changed"]).toBeDefined());
    act(() => {
      for (const listener of listeners["tasks:changed"] ?? []) listener({ payload: ["task-1"] });
    });

    await waitFor(() => expect(result.current.worktrees.has("task-1")).toBe(false));
    expect(mockInvoke).toHaveBeenCalledWith("list_local_worktrees", undefined);
  });
});
