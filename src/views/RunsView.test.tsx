import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { RunsView } from "./RunsView";
import type { Repository, TaskSummary } from "../types";

// Mocked at the Tauri seam, not `lib/commands.ts`/`lib/events.ts` — see
// `StorageSection.test.tsx`'s own comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function taskSummary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    repositoryId: "repo-1",
    title: "Wire up the board",
    plan: "a plan",
    extraInstructions: null,
    column: "ready",
    position: 0,
    runState: "running",
    branch: "rimaia/task-1",
    worktreePath: "/data/worktrees/repo/task-1",
    strategyMode: "default",
    model: null,
    effort: null,
    strategyPlan: null,
    strategySource: null,
    strategyUpdatedAt: null,
    createdAt: "2026-08-20T11:00:00Z",
    updatedAt: "2026-08-20T11:55:00Z",
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    lastRun: { status: "running", exitClass: null, endedAt: null },
    ...overrides,
  };
}

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees/rimaia",
    allowUnattendedRuns: true,
    createdAt: "2026-08-20T09:00:00Z",
    ...overrides,
  };
}

/** Every test's default backend: no running tasks, one repository, and every
 *  `listen` subscription resolves and never fires on its own — the same
 *  shape `Board.test.tsx`'s own `mockBackend` uses. `get_task`/`get_run_tail`
 *  are answered too, because `ActiveRunCard` (rendered per running task)
 *  calls both independently of anything this view does itself. */
function mockBackend({
  runningTasks = [] as TaskSummary[],
  repositories = [repository()],
  runEnvironment = "inherit" as "inherit" | "strict_local",
} = {}) {
  const listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
  const unlistenSpies: Record<string, ReturnType<typeof vi.fn>> = {};

  mockListen.mockImplementation(async (eventName, callback) => {
    listenHandlers[eventName as string] = callback as (event: { payload: unknown }) => void;
    const unlisten = vi.fn();
    unlistenSpies[eventName as string] = unlisten;
    return unlisten;
  });

  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "list_tasks") {
      expect(args).toEqual({ filter: { runState: "running" } });
      return runningTasks;
    }
    if (command === "list_repositories") return repositories;
    if (command === "get_run_environment") return runEnvironment;
    if (command === "get_task") {
      const taskId = (args as { id: string }).id;
      const task = runningTasks.find((t) => t.id === taskId);
      return {
        ...task,
        links: [],
        dependsOn: [],
        lastRun: {
          id: `run-for-${taskId}`,
          taskId,
          attempt: 1,
          status: "running",
          sessionId: "session-1",
          prompt: "prompt",
          startedAt: "2026-08-20T11:55:00Z",
          endedAt: null,
          exitClass: null,
          errorMessage: null,
          numTurns: null,
          costUsd: null,
          logPath: `/data/runs/${taskId}/run.jsonl`,
          prUrl: null,
          resumeAfter: null,
        },
      };
    }
    if (command === "get_run_tail") return null;
    throw new Error(`unexpected command: ${command}`);
  });

  return { listenHandlers, unlistenSpies };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
});

describe("RunsView", () => {
  it("shows the empty state when nothing is running", async () => {
    mockBackend({ runningTasks: [] });
    render(<RunsView />);

    expect(await screen.findByText("Nothing running right now")).toBeInTheDocument();
  });

  it("renders a card for each task list_tasks reports as running", async () => {
    mockBackend({ runningTasks: [taskSummary()] });
    render(<RunsView />);

    expect(await screen.findByText("Wire up the board")).toBeInTheDocument();
    expect(screen.queryByText("Nothing running right now")).toBeNull();
  });

  it("shows the current run_environment setting", async () => {
    mockBackend({ runningTasks: [], runEnvironment: "strict_local" });
    render(<RunsView />);

    expect(await screen.findByText(/Strict \/ local/)).toBeInTheDocument();
  });

  it("re-reads the running-task list on tasks:changed", async () => {
    let call = 0;
    const { listenHandlers } = mockBackend({ runningTasks: [] });
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_tasks") {
        call += 1;
        return call === 1 ? [] : [taskSummary()];
      }
      if (command === "list_repositories") return [repository()];
      if (command === "get_run_environment") return "inherit";
      if (command === "get_task") {
        const taskId = (args as { id: string }).id;
        return {
          ...taskSummary({ id: taskId }),
          links: [],
          dependsOn: [],
          lastRun: null,
        };
      }
      if (command === "get_run_tail") return null;
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunsView />);
    await waitFor(() => expect(call).toBe(1));
    expect(screen.getByText("Nothing running right now")).toBeInTheDocument();

    act(() => listenHandlers["tasks:changed"]?.({ payload: ["task-1"] }));

    expect(await screen.findByText("Wire up the board")).toBeInTheDocument();
  });

  it("unsubscribes from tasks:changed on unmount", async () => {
    const { unlistenSpies } = mockBackend({ runningTasks: [] });
    const { unmount } = render(<RunsView />);

    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function)),
    );

    unmount();

    expect(unlistenSpies["tasks:changed"]).toHaveBeenCalled();
  });

  it("shows the backend's own error when list_tasks rejects", async () => {
    mockListen.mockResolvedValue(vi.fn());
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") {
        throw { code: "internal", message: "the database is unavailable" };
      }
      if (command === "list_repositories") return [repository()];
      if (command === "get_run_environment") return "inherit";
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunsView />);

    expect(await screen.findByText("the database is unavailable")).toBeInTheDocument();
  });
});
