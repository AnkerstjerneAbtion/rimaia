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

  // The bug this pins: the overlay used to render where it is written —
  // inside `.task-detail-panel`, which is a scroll container and its own
  // stacking context. WKWebView lays a `position: fixed` element out against
  // an ancestor like that rather than against the viewport, so opening a run
  // from the board painted the detail at the top of a panel the reader had
  // already scrolled past: the panel blanked and nothing appeared. Portalled
  // to `document.body`, where it is written no longer decides where it is
  // painted, and this mount point behaves like the Runs view's.
  it("renders the overlay outside the panel's own subtree, portalled to the body", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_runs_for_task") return [run()];
      if (command === "get_run") {
        return {
          ...run(),
          diff: { taskId: "task-1", branch: null, baseRef: "main", diff: { filesChanged: 0, insertions: 0, deletions: 0 }, files: [], commits: [] },
          logAvailable: false,
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    const { container } = render(<RunHistorySection taskId="task-1" />);

    fireEvent.click(await screen.findByText("Attempt 1"));

    const overlay = await screen.findByRole("dialog", { name: "Run detail" });
    expect(container.querySelector(".run-history-section")).not.toContainElement(overlay);
    expect(overlay.parentElement).toBe(document.body);
  });

  it("prunes this task's logs once the deletion is confirmed, and reports what was removed", async () => {
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
    fireEvent.click(await screen.findByRole("button", { name: "Delete transcripts" }));

    expect(await screen.findByText("Removed 1 log, freed 2.0 KB.")).toBeInTheDocument();
  });

  // The house rule for anything that deletes files: the dangerous step is the
  // one the user names (`worktree::ForceRemoval::ConfirmedByUser`). The first
  // click must not have reached the backend.
  it("deletes nothing until the prune is confirmed, and cancelling deletes nothing at all", async () => {
    const commands: string[] = [];
    mockInvoke.mockImplementation(async (command) => {
      commands.push(command as string);
      if (command === "list_runs_for_task") return [run()];
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunHistorySection taskId="task-1" />);

    fireEvent.click(await screen.findByRole("button", { name: "Prune this task's logs" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(commands).not.toContain("prune_run_logs");
    expect(
      await screen.findByRole("button", { name: "Prune this task's logs" }),
    ).toBeInTheDocument();
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
