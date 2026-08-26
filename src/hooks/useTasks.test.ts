import { StrictMode } from "react";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { useTasks } from "./useTasks";
import type { Task } from "../types";

// Mocked at the Tauri seam (`commands.ts` and `events.ts`'s own boundary),
// not the wrapper modules — see `StorageSection.test.tsx`'s comment.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    repositoryId: "repo-1",
    title: "Wire the board",
    plan: "a plan",
    extraInstructions: null,
    column: "ready",
    position: 0,
    runState: "idle",
    branch: null,
    worktreePath: null,
    strategyMode: "default",
    model: null,
    effort: null,
    strategyPlan: null,
    strategySource: null,
    strategyUpdatedAt: null,
    createdAt: "2026-08-20T11:00:00Z",
    updatedAt: "2026-08-20T11:00:00Z",
    source: "ui",
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("useTasks: stale refresh response", () => {
  it("keeps the newest list_tasks response even when an older request resolves later", async () => {
    // Fix pass finding 1: `refresh()` had no generation guard, so whichever
    // read resolved *last* won, regardless of which was issued last.
    const unfiltered = [
      task({ id: "a", repositoryId: "repo-1" }),
      task({ id: "b", repositoryId: "repo-2" }),
    ];
    const filtered = [task({ id: "b", repositoryId: "repo-2" })];

    const calls: unknown[] = [];
    let resolveFirst: (tasks: Task[]) => void = () => {};
    let resolveSecond: (tasks: Task[]) => void = () => {};

    mockInvoke.mockImplementation(async (command, args) => {
      if (command !== "list_tasks") throw new Error(`unexpected command: ${command}`);
      calls.push(args);
      if (calls.length === 1) {
        return new Promise<Task[]>((resolve) => {
          resolveFirst = resolve;
        });
      }
      return new Promise<Task[]>((resolve) => {
        resolveSecond = resolve;
      });
    });

    const { result, rerender } = renderHook(
      ({ repositoryId }: { repositoryId: string | null }) => useTasks(repositoryId),
      { initialProps: { repositoryId: null as string | null } },
    );

    await waitFor(() => expect(calls).toHaveLength(1));
    rerender({ repositoryId: "repo-2" });
    await waitFor(() => expect(calls).toHaveLength(2));

    // The newer request (switching to repo-2) resolves first; the stale,
    // unfiltered request resolves after it. The stale response must not
    // overwrite the newer one that already landed.
    await act(async () => resolveSecond(filtered));
    await waitFor(() => expect(result.current.state.tasks).toEqual(filtered));

    await act(async () => resolveFirst(unfiltered));
    expect(result.current.state.tasks).toEqual(filtered);
  });
});

describe("useTasks: moveCard purity", () => {
  it("sends exactly one move_task call per drag, even under StrictMode's double-invoked updaters", async () => {
    // Fix pass finding 2: planning and dispatching the move from inside a
    // `setState` updater made the updater impure (it called `moveTask`, an
    // IPC command), and StrictMode double-invokes updaters in dev
    // (`main.tsx` renders under `<React.StrictMode>`), doubling every drag's
    // `move_task` call.
    const tasks = [task({ id: "a", position: 0 }), task({ id: "b", position: 1 })];
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") return tasks;
      throw new Error(`unexpected command: ${command}`);
    });

    const { result } = renderHook(() => useTasks(null), { wrapper: StrictMode });
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(2));

    const moveCalls: unknown[] = [];
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_tasks") return tasks;
      if (command === "move_task") {
        moveCalls.push(args);
        return { ...tasks[0], position: 5, updatedAt: "2026-08-20T12:00:00Z" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    act(() => {
      result.current.moveCard("a", "ready", 2);
    });

    await waitFor(() => expect(result.current.state.pending).toHaveLength(0));
    expect(moveCalls).toHaveLength(1);
  });
});

describe("useTasks: settlement reconciliation", () => {
  it("re-reads when the settled row implies the destination column was rebalanced", async () => {
    // `a` and `b` sit close enough together that dropping `z` between them
    // is exactly `settlementReproducesMove`'s own rebalance case (see
    // `lib/board.test.ts`): `move_task` answers with `z`'s row alone, which
    // is not enough to know the whole column was renumbered, so the hook
    // falls back to a full re-read.
    const tasks = [
      task({ id: "z", column: "not_ready", position: 0 }),
      task({ id: "a", column: "ready", position: 1 }),
      task({ id: "b", column: "ready", position: 1.0000001 }),
    ];
    const calls: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      calls.push(command as string);
      if (command === "list_tasks") return tasks;
      throw new Error(`unexpected command: ${command}`);
    });

    const { result } = renderHook(() => useTasks(null));
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(3));

    mockInvoke.mockImplementation(async (command) => {
      calls.push(command as string);
      if (command === "list_tasks") return tasks;
      if (command === "move_task") return { ...task({ id: "z", column: "ready" }), position: 0.5 };
      throw new Error(`unexpected command: ${command}`);
    });

    act(() => {
      result.current.moveCard("z", "ready", 1);
    });

    await waitFor(() => expect(result.current.state.pending).toHaveLength(0));
    expect(calls.filter((command) => command === "list_tasks")).toHaveLength(2);
  });

  it("does not re-read when the settled row's neighbours reproduce the drop", async () => {
    const tasks = [
      task({ id: "z", column: "not_ready", position: 0 }),
      task({ id: "a", column: "ready", position: 0 }),
      task({ id: "b", column: "ready", position: 1 }),
    ];
    const calls: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      calls.push(command as string);
      if (command === "list_tasks") return tasks;
      throw new Error(`unexpected command: ${command}`);
    });

    const { result } = renderHook(() => useTasks(null));
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(3));

    mockInvoke.mockImplementation(async (command) => {
      calls.push(command as string);
      if (command === "list_tasks") return tasks;
      if (command === "move_task") return { ...task({ id: "z", column: "ready" }), position: 0.5 };
      throw new Error(`unexpected command: ${command}`);
    });

    act(() => {
      result.current.moveCard("z", "ready", 1);
    });

    await waitFor(() => expect(result.current.state.pending).toHaveLength(0));
    expect(calls.filter((command) => command === "list_tasks")).toHaveLength(1);
  });
});
