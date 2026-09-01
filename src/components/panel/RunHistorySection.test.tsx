import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { RunHistorySection } from "./RunHistorySection";
import type { Run } from "../../types";

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

function run(overrides: Partial<Run> = {}): Run {
  return {
    id: "run-1",
    taskId: "task-1",
    attempt: 1,
    status: "succeeded",
    sessionId: "session-1",
    prompt: "do the thing",
    startedAt: "2026-08-20T11:00:00Z",
    endedAt: "2026-08-20T11:30:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 4,
    costUsd: 0.05,
    logPath: "/data/runs/task-1/run-1.jsonl",
    prUrl: null,
    resumeAfter: null,
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("RunHistorySection", () => {
  it("shows the empty case when the task has never run", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);

    expect(await screen.findByText(/No runs yet/)).toBeInTheDocument();
  });

  it("lists every attempt, newest first as list_runs_for_task orders them", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_runs_for_task") {
        return [run({ id: "run-2", attempt: 2, exitClass: "fatal" }), run({ id: "run-1", attempt: 1 })];
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);

    expect(await screen.findByText("Attempt 2")).toBeInTheDocument();
    expect(screen.getByText("Attempt 1")).toBeInTheDocument();
  });

  it("opens the run detail overlay when an attempt is clicked", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_runs_for_task") return [run()];
      if (command === "get_run") {
        expect((args as { runId: string }).runId).toBe("run-1");
        return {
          ...run(),
          diff: { taskId: "task-1", branch: null, baseRef: "main", diff: { filesChanged: 0, insertions: 0, deletions: 0 }, files: [], commits: [] },
          logAvailable: true,
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);

    fireEvent.click(await screen.findByText("Attempt 1"));

    expect(await screen.findByRole("dialog", { name: "Run detail" })).toBeInTheDocument();
  });

  it("prunes this task's logs and reports what was removed", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_runs_for_task") return [run()];
      if (command === "prune_run_logs") {
        expect(args).toEqual({ criterion: { kind: "task", taskId: "task-1" } });
        return { runsPruned: 1, bytesFreed: 2048 };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);

    fireEvent.click(await screen.findByRole("button", { name: "Prune this task's logs" }));

    expect(await screen.findByText("Removed 1 log, freed 2.0 KB.")).toBeInTheDocument();
  });

  it("re-reads the history on runs:changed", async () => {
    let call = 0;
    const listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
    mockListen.mockImplementation(async (eventName, callback) => {
      listenHandlers[eventName as string] = callback as (event: { payload: unknown }) => void;
      return vi.fn();
    });
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_runs_for_task") {
        call += 1;
        return call === 1 ? [] : [run()];
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);
    await waitFor(() => expect(call).toBe(1));
    expect(screen.getByText(/No runs yet/)).toBeInTheDocument();

    listenHandlers["runs:changed"]?.({ payload: ["run-1"] });

    expect(await screen.findByText("Attempt 1")).toBeInTheDocument();
  });
});
