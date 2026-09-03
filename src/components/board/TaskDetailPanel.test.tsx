import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { TaskDetailPanel } from "./TaskDetailPanel";
import type { Task, TaskDetail, WorktreeStatus } from "../../types";

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
    id: "task-1",
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

/** The three `effective*` fields are `get_task`'s own, not columns (D12's
 *  task 020 amendment): a `TaskDetail` without them is not something the
 *  backend can return, so the fixture carries the "nothing configured
 *  anywhere" answer rather than leaving them out. */
function detail(overrides: Partial<TaskDetail> = {}): TaskDetail {
  return {
    ...task(),
    links: [],
    dependsOn: [],
    lastRun: null,
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
    ...overrides,
  };
}

/** `WorktreeSection` (task 007) fetches this independently of `get_task` —
 *  every test below has to answer `get_worktree_status` too, or the
 *  section's own request rejects with "unexpected command" and renders an
 *  error banner none of these tests are about. Defaults to the deliberate
 *  "never run" empty case, which is what every fixture `task()`/`detail()`
 *  above already implies (`worktreePath: null`). */
function worktreeStatus(overrides: Partial<WorktreeStatus> = {}): WorktreeStatus {
  return {
    taskId: "task-1",
    exists: false,
    path: null,
    branch: null,
    baseRef: "main",
    dependencyWarning: null,
    ahead: 0,
    behind: 0,
    dirty: false,
    commitCount: 0,
    diff: { filesChanged: 0, insertions: 0, deletions: 0 },
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

describe("TaskDetailPanel", () => {
  it("fetches and renders the task's detail, including its sections", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return detail();
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    render(
      <TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />,
    );

    expect(mockInvoke).toHaveBeenCalledWith("get_task", { id: "task-1" });
    expect(await screen.findByText("No links yet.")).toBeInTheDocument();
    expect(screen.getByLabelText("Task title")).toHaveValue("Wire the board");
    expect(screen.getByText("rimaia")).toBeInTheDocument();
    expect(screen.getByLabelText("Plan")).toHaveValue("a plan");
    expect(screen.getByText("Delete task")).toBeInTheDocument();
  });

  it("shows what a run would spawn with, resolved by the backend (task 020)", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        return detail({
          effectiveModel: "sonnet",
          effectiveEffort: "high",
          effectiveOrigin: "repository",
        });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      if (command === "get_strategy_catalogue") {
        return {
          catalogue: { models: [], efforts: [], planner: { model: null, effort: null, max_turns: 0 } },
          json: "{}",
          defaultJson: "{}",
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />);

    // The effective triple rides along on `get_task` — the section never
    // resolves the precedence chain itself (seam-contract D12's amendment).
    expect(
      await screen.findByText("Runs as Sonnet · high — from the repository default."),
    ).toBeInTheDocument();
  });

  it("shows the last run's outcome, cost included, once detail resolves (task 008)", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        return detail({
          lastRun: {
            id: "run-1",
            taskId: "task-1",
            attempt: 1,
            status: "succeeded",
            sessionId: "s1",
            prompt: "p",
            startedAt: "2026-08-20T09:00:00Z",
            endedAt: "2026-08-20T09:10:00Z",
            exitClass: "success",
            errorMessage: null,
            numTurns: 5,
            costUsd: 0.1061,
            logPath: "/data/runs/task-1/run-1.jsonl",
            prUrl: null,
            resumeAfter: null,
            baseRef: null,
            model: null,
            effort: null,
            runEnvironment: null,
            inputTokens: null,
            outputTokens: null,
            cacheReadTokens: null,
            cacheCreationTokens: null,
          },
        });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />);

    expect(await screen.findByText("$0.1061")).toBeInTheDocument();
    // "Succeeded" legitimately appears twice once detail resolves —
    // `RunInfoSection`'s own status label (task 007) and this section's exit
    // class both read it off the same fetched run — so this scopes to the
    // section task 008 owns rather than picking one arbitrarily.
    const outcomeSection = document.querySelector<HTMLElement>(".run-outcome-section")!;
    expect(within(outcomeSection).getByText("Succeeded")).toBeInTheDocument();
    expect(screen.getByText("/data/runs/task-1/run-1.jsonl")).toBeInTheDocument();
  });

  it("remounts every field fresh when the selected task changes", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_task") {
        return (args as { id: string }).id === "task-1"
          ? detail({ id: "task-1" })
          : detail({ id: "task-2", plan: "a different plan" });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    const { rerender } = render(
      <TaskDetailPanel task={task({ id: "task-1", plan: "a plan" })} repositoryName="rimaia" onClose={vi.fn()} />,
    );
    expect(await screen.findByLabelText("Plan")).toHaveValue("a plan");

    rerender(
      <TaskDetailPanel
        task={task({ id: "task-2", title: "Other task", plan: "a different plan" })}
        repositoryName="rimaia"
        onClose={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getByLabelText("Plan")).toHaveValue("a different plan"));
    expect(screen.getByLabelText("Task title")).toHaveValue("Other task");
  });

  it("reads interrupted off the fetched last run once detail resolves (D9)", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        return detail({
          runState: "failed",
          lastRun: {
            id: "run-1",
            taskId: "task-1",
            attempt: 1,
            status: "interrupted",
            sessionId: "s1",
            prompt: "p",
            startedAt: "2026-08-20T09:00:00Z",
            endedAt: "2026-08-20T09:05:00Z",
            exitClass: "interrupted",
            errorMessage: null,
            numTurns: 1,
            costUsd: null,
            logPath: "/tmp/run.jsonl",
            prUrl: null,
            resumeAfter: null,
            baseRef: null,
            model: null,
            effort: null,
            runEnvironment: null,
            inputTokens: null,
            outputTokens: null,
            cacheReadTokens: null,
            cacheCreationTokens: null,
          },
        });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    render(
      <TaskDetailPanel
        task={task({ runState: "failed" })}
        repositoryName="rimaia"
        onClose={vi.fn()}
      />,
    );

    // Two "Interrupted"s legitimately appear once detail resolves — the
    // run-state badge and the "Last run" outcome both read it off the same
    // fetched run — so this waits for the badge specifically.
    await waitFor(() =>
      expect(document.querySelector(".run-badge-interrupted")).toBeInTheDocument(),
    );
  });

  it("refetches detail when tasks:changed names this task's id", async () => {
    let resolveCount = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        resolveCount += 1;
        return detail({ plan: resolveCount === 1 ? "first" : "second" });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    let changedHandler: ((ids: string[]) => void) | undefined;
    mockListen.mockImplementation(async (_name, callback) => {
      changedHandler = (ids: string[]) => callback({ payload: ids } as never);
      return vi.fn();
    });

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />);
    await screen.findByText("No links yet.");

    changedHandler?.(["task-1"]);

    // Counts only `get_task` calls, not every `invoke` call: `WorktreeSection`
    // (task 007) also subscribes to `tasks:changed` and fetches its own
    // `get_worktree_status` on the same trigger, which a bare total call
    // count would conflate with the detail refetch this test is about.
    await waitFor(() =>
      expect(mockInvoke.mock.calls.filter(([command]) => command === "get_task")).toHaveLength(2),
    );
  });

  it("refetches on an empty tasks:changed payload — ADR-0018's lag-recovery case means re-read everything", async () => {
    let resolveCount = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        resolveCount += 1;
        return detail({ plan: resolveCount === 1 ? "first" : "second" });
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    let changedHandler: ((ids: string[]) => void) | undefined;
    mockListen.mockImplementation(async (_name, callback) => {
      changedHandler = (ids: string[]) => callback({ payload: ids } as never);
      return vi.fn();
    });

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />);
    await screen.findByText("No links yet.");

    changedHandler?.([]);

    // Counts only `get_task` calls, not every `invoke` call: `WorktreeSection`
    // (task 007) also subscribes to `tasks:changed` and fetches its own
    // `get_worktree_status` on the same trigger, which a bare total call
    // count would conflate with the detail refetch this test is about.
    await waitFor(() =>
      expect(mockInvoke.mock.calls.filter(([command]) => command === "get_task")).toHaveLength(2),
    );
  });

  it("does not refetch when tasks:changed names a different task's id", async () => {
    let getTaskCalls = 0;
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") {
        getTaskCalls += 1;
        return detail();
      }
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    let changedHandler: ((ids: string[]) => void) | undefined;
    mockListen.mockImplementation(async (_name, callback) => {
      changedHandler = (ids: string[]) => callback({ payload: ids } as never);
      return vi.fn();
    });

    render(<TaskDetailPanel task={task({ id: "task-1" })} repositoryName="rimaia" onClose={vi.fn()} />);
    await screen.findByText("No links yet.");
    expect(getTaskCalls).toBe(1);

    changedHandler?.(["some-other-task"]);

    // Nothing to await for — a refetch would be a second synchronous
    // `get_task` call, so a settled promise queue with the count unchanged
    // is proof it did not happen, not just that it hasn't happened yet.
    await Promise.resolve();
    expect(getTaskCalls).toBe(1);
  });

  it("restores the previous title rather than saving a blank one", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return detail();
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      throw new Error(`unexpected command: ${command}`);
    });

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={vi.fn()} />);
    const titleInput = screen.getByLabelText("Task title");
    await waitFor(() => expect(titleInput).toHaveValue("Wire the board"));

    fireEvent.change(titleInput, { target: { value: "   " } });
    fireEvent.blur(titleInput);

    // `title` is `NOT NULL` (core's own rule: "a task's title must not be
    // blank") - nothing is sent, and the field is restored rather than left
    // visibly blank with no save. Both halves matter: restoring the value
    // silently would leave a keyboard user with no idea why their edit did
    // not stick, so core's own refusal sentence has to surface too, not just
    // the input's value snapping back.
    expect(mockInvoke).not.toHaveBeenCalledWith("update_task", expect.anything());
    expect(titleInput).toHaveValue("Wire the board");
    expect(
      await screen.findByText("a task's title must not be blank"),
    ).toBeInTheDocument();
  });

  it("deletes the task after confirmation and closes the panel", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_task") return detail();
      if (command === "get_worktree_status") return worktreeStatus();
      if (command === "list_runs_for_task") return [];
      if (command === "delete_task") return undefined;
      throw new Error(`unexpected command: ${command}`);
    });
    const onClose = vi.fn();

    render(<TaskDetailPanel task={task()} repositoryName="rimaia" onClose={onClose} />);
    await screen.findByText("No links yet.");

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));
    expect(mockInvoke).not.toHaveBeenCalledWith("delete_task", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Delete task" }));

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(mockInvoke).toHaveBeenCalledWith("delete_task", { id: "task-1" });
  });
});
