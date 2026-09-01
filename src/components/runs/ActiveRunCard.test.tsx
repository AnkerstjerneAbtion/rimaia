import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ActiveRunCard, formatElapsed } from "./ActiveRunCard";
import type { RunTail, TaskDetail, TaskSummary } from "../../types";

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
    source: "ui",
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    lastRun: { status: "running", exitClass: null, endedAt: null },
    // Nothing configured anywhere, which is what a card with no strategy
    // shows: the badge renders nothing rather than "undefined".
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
    ...overrides,
  };
}

function taskDetail(overrides: Partial<TaskDetail> = {}): TaskDetail {
  return {
    ...taskSummary(),
    links: [],
    dependsOn: [],
    lastRun: {
      id: "run-1",
      taskId: "task-1",
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
      logPath: "/data/runs/task-1/run-1.jsonl",
      prUrl: null,
      resumeAfter: null,
    },
    ...overrides,
  };
}

function runTail(overrides: Partial<RunTail> = {}): RunTail {
  return {
    runId: "run-1",
    elapsedMs: 65_000,
    turns: 3,
    currentTool: { id: "tool-1", name: "Bash", detail: "npm test" },
    lastAssistantText: "Running the test suite now.",
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("formatElapsed", () => {
  it("shows only seconds under a minute", () => {
    expect(formatElapsed(45_000)).toBe("45s");
  });

  it("floors rather than rounds a run just under a minute old", () => {
    expect(formatElapsed(59_900)).toBe("59s");
  });

  it("shows minutes and zero-padded seconds at a minute and beyond", () => {
    expect(formatElapsed(65_000)).toBe("1m 05s");
  });
});

describe("ActiveRunCard", () => {
  it("resolves its run id from get_task, seeds from get_run_tail, and renders the snapshot", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") {
        expect(args).toEqual({ runId: "run-1" });
        return runTail();
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);

    expect(screen.getByText("Wire up the board")).toBeInTheDocument();
    expect(screen.getByText("rimaia")).toBeInTheDocument();
    expect(await screen.findByText("1m 05s")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
    expect(screen.getByText("Bash")).toBeInTheDocument();
    expect(screen.getByText(/npm test/)).toBeInTheDocument();
    expect(screen.getByText("Running the test suite now.")).toBeInTheDocument();
  });

  it("shows a starting placeholder before the run id has resolved", async () => {
    let resolveGetTask: (detail: TaskDetail) => void = () => {};
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        return new Promise((resolve) => {
          resolveGetTask = resolve;
        });
      }
      if (command === "get_run_tail") return runTail();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);

    expect(screen.getByText("Starting…")).toBeInTheDocument();
    expect(screen.getByText("Nothing yet.")).toBeInTheDocument();

    resolveGetTask(taskDetail());
    await waitFor(() => expect(screen.queryByText("Starting…")).toBeNull());
  });

  it("retries get_task on the next tasks:changed after the first lookup fails", async () => {
    let getTaskCalls = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        getTaskCalls += 1;
        if (getTaskCalls === 1) throw new Error("boom");
        return taskDetail();
      }
      if (command === "get_run_tail") return runTail();
      throw new Error(`unexpected command: ${command}`);
    });

    let tasksChangedHandler: ((event: { payload: string[] }) => void) | undefined;
    mockListen.mockImplementation(async (eventName, callback) => {
      if (eventName === "tasks:changed") tasksChangedHandler = callback as never;
      return vi.fn();
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);

    await waitFor(() => expect(getTaskCalls).toBe(1));
    expect(screen.getByText("Starting…")).toBeInTheDocument();

    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function)),
    );
    act(() => tasksChangedHandler?.({ payload: ["task-1"] }));

    await waitFor(() => expect(screen.queryByText("Starting…")).toBeNull());
    expect(getTaskCalls).toBe(2);
  });

  it("ignores a tasks:changed publish for an unrelated task while unresolved", async () => {
    let getTaskCalls = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        getTaskCalls += 1;
        return new Promise(() => {
          // Never resolves — this test only checks whether a second call
          // happens, not what it returns.
        });
      }
      if (command === "get_run_tail") return runTail();
      throw new Error(`unexpected command: ${command}`);
    });

    let tasksChangedHandler: ((event: { payload: string[] }) => void) | undefined;
    mockListen.mockImplementation(async (eventName, callback) => {
      if (eventName === "tasks:changed") tasksChangedHandler = callback as never;
      return vi.fn();
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);

    await waitFor(() => expect(getTaskCalls).toBe(1));
    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function)),
    );

    act(() => tasksChangedHandler?.({ payload: ["some-other-task"] }));

    expect(getTaskCalls).toBe(1);
  });

  it("establishes the runs:tail subscription and updates the view from a matching snapshot", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") return null;
      throw new Error(`unexpected command: ${command}`);
    });

    let tailHandler: ((event: { payload: RunTail }) => void) | undefined;
    mockListen.mockImplementation(async (eventName, callback) => {
      if (eventName === "runs:tail") tailHandler = callback as never;
      return vi.fn();
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);

    await waitFor(() => expect(mockListen).toHaveBeenCalledWith("runs:tail", expect.any(Function)));
    expect(tailHandler).toBeTypeOf("function");

    act(() => tailHandler?.({ payload: runTail({ elapsedMs: 130_000, turns: 4 }) }));

    expect(await screen.findByText("2m 10s")).toBeInTheDocument();
    expect(screen.getByText("4")).toBeInTheDocument();
  });

  it("ignores a runs:tail snapshot for a different run id", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") return runTail({ elapsedMs: 65_000, turns: 3 });
      throw new Error(`unexpected command: ${command}`);
    });

    let tailHandler: ((event: { payload: RunTail }) => void) | undefined;
    mockListen.mockImplementation(async (eventName, callback) => {
      if (eventName === "runs:tail") tailHandler = callback as never;
      return vi.fn();
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);
    expect(await screen.findByText("1m 05s")).toBeInTheDocument();

    // `act()`, not a bare call or a microtask wait — `setTail` inside the
    // subscription handler is synchronous, so anything a broken guard lets
    // through is already flushed to the DOM before the next line runs. A
    // handler invoked outside `act()` (or only awaited past a single
    // microtask) can leave the update pending when the assertion below
    // reads the DOM, which would pass regardless of whether the guard did
    // its job.
    act(() =>
      tailHandler?.({ payload: runTail({ runId: "some-other-run", elapsedMs: 999_000, turns: 99 }) }),
    );

    expect(screen.getByText("1m 05s")).toBeInTheDocument();
    expect(screen.queryByText("99")).toBeNull();
  });

  it("calls the unlisten handle returned by runs:tail on unmount", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") return null;
      throw new Error(`unexpected command: ${command}`);
    });

    const unlisten = vi.fn();
    mockListen.mockImplementation(async (eventName) => {
      if (eventName === "runs:tail") return unlisten;
      return vi.fn();
    });

    const { unmount } = render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);
    await waitFor(() => expect(mockListen).toHaveBeenCalledWith("runs:tail", expect.any(Function)));

    unmount();

    expect(unlisten).toHaveBeenCalled();
  });

  it("calls cancel_task_run with the task id when Cancel is clicked", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") return runTail();
      if (command === "cancel_task_run") return undefined;
      throw new Error(`unexpected command: ${command}`);
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);
    await screen.findByText("1m 05s");

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(mockInvoke).toHaveBeenCalledWith("cancel_task_run", { taskId: "task-1" });
    await waitFor(() => expect(screen.getByRole("button", { name: "Cancel" })).toBeEnabled());
  });

  it("shows the backend's own rejection message when cancel_task_run fails", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return taskDetail();
      if (command === "get_run_tail") return runTail();
      if (command === "cancel_task_run") {
        throw { code: "internal", message: "could not signal the run's process group" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<ActiveRunCard task={taskSummary()} repositoryName="rimaia" />);
    await screen.findByText("1m 05s");

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(
      await screen.findByText("could not signal the run's process group"),
    ).toBeInTheDocument();
  });
});
