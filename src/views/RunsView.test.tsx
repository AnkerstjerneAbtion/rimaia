import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { RunsView } from "./RunsView";
import type { QueueStatus, Repository, RunListEntry, TaskDetail, TaskSummary } from "../types";

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
    blockingTitle: null,
    lastRun: { status: "running", exitClass: null, endedAt: null },
    // Nothing configured anywhere, which is what a card with no strategy
    // shows: the badge renders nothing rather than "undefined".
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
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

function queueStatus(overrides: Partial<QueueStatus> = {}): QueueStatus {
  return {
    state: "paused",
    runningTaskId: null,
    plan: [],
    lastStepError: null,
    ...overrides,
  };
}

/** A `get_task` response for `taskId`, built the same shape `mockBackend`'s
 *  own default gives a running task — reused directly by tests that need a
 *  *finished* run's own `lastRun`, so only the fields a test cares about have
 *  to be overridden. */
function taskDetailFor(taskId: string, overrides: Partial<TaskDetail> = {}): TaskDetail {
  return {
    ...taskSummary({ id: taskId }),
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
      baseRef: null,
      model: null,
      effort: null,
      runEnvironment: null,
      inputTokens: null,
      outputTokens: null,
      cacheReadTokens: null,
      cacheCreationTokens: null,
    },
    ...overrides,
  };
}

function runListEntry(overrides: Partial<RunListEntry> = {}): RunListEntry {
  return {
    id: "run-1",
    taskId: "task-1",
    attempt: 1,
    status: "succeeded",
    sessionId: "session-1",
    prompt: "prompt",
    startedAt: "2026-08-20T11:00:00Z",
    endedAt: "2026-08-20T11:30:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 4,
    costUsd: 0.05,
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
    taskTitle: "Wire up the board",
    repositoryId: "repo-1",
    repositoryName: "rimaia",
    logAvailable: true,
    ...overrides,
  };
}

/** Every test's default backend: no running tasks, one repository, a paused
 *  empty queue, and every `listen` subscription resolves and never fires on
 *  its own — the same shape `Board.test.tsx`'s own `mockBackend` uses.
 *  `get_task`/`get_run_tail` are answered too, because `ActiveRunCard`
 *  (rendered per running task) calls both independently of anything this
 *  view does itself. */
function mockBackend({
  runningTasks = [] as TaskSummary[],
  repositories = [repository()],
  runEnvironment = "inherit" as "inherit" | "strict_local",
  queue = queueStatus(),
  historyEntries = [] as RunListEntry[],
} = {}) {
  // Arrays, not a bare handler per name: task 009 adds a second `tasks:changed`
  // subscriber (the queue-status effect, alongside the pre-existing
  // running-tasks one), and a real Tauri `emit` notifies every listener, not
  // just the most recently registered one.
  const listenHandlers: Record<string, Array<(event: { payload: unknown }) => void>> = {};
  const unlistenSpies: Record<string, ReturnType<typeof vi.fn>[]> = {};

  mockListen.mockImplementation(async (eventName, callback) => {
    const name = eventName as string;
    (listenHandlers[name] ??= []).push(callback as (event: { payload: unknown }) => void);
    const unlisten = vi.fn();
    (unlistenSpies[name] ??= []).push(unlisten);
    return unlisten;
  });

  /** Invokes every listener registered for `eventName`, the way a real
   *  `app.emit` would reach every subscriber (ADR-0018). */
  function fire(eventName: string, payload: unknown) {
    for (const handler of listenHandlers[eventName] ?? []) handler({ payload });
  }

  mockInvoke.mockImplementation(async (command, args) => {
    if (command === "list_tasks") {
      expect(args).toEqual({ filter: { runState: "running" } });
      return runningTasks;
    }
    if (command === "list_repositories") return repositories;
    if (command === "get_run_environment") return runEnvironment;
    if (command === "get_queue_status") return queue;
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
          baseRef: null,
          model: null,
          effort: null,
          runEnvironment: null,
          inputTokens: null,
          outputTokens: null,
          cacheReadTokens: null,
          cacheCreationTokens: null,
        },
      };
    }
    if (command === "get_run_tail") return null;
    if (command === "list_runs") return historyEntries;
    throw new Error(`unexpected command: ${command}`);
  });

  return { listenHandlers, unlistenSpies, fire };
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
    const { fire } = mockBackend({ runningTasks: [] });
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_tasks") {
        call += 1;
        return call === 1 ? [] : [taskSummary()];
      }
      if (command === "list_repositories") return [repository()];
      if (command === "get_run_environment") return "inherit";
      if (command === "get_queue_status") return queueStatus();
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

    act(() => fire("tasks:changed", ["task-1"]));

    expect(await screen.findByText("Wire up the board")).toBeInTheDocument();
  });

  it("unsubscribes from every tasks:changed listener on unmount", async () => {
    // Two listeners share this event name — the pre-existing running-tasks
    // effect and task 009's queue-status effect — and both have to be told to
    // stop, not just whichever registered first.
    const { unlistenSpies } = mockBackend({ runningTasks: [] });
    const { unmount } = render(<RunsView />);

    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function)),
    );

    unmount();

    expect(unlistenSpies["tasks:changed"]?.length).toBeGreaterThanOrEqual(2);
    for (const unlisten of unlistenSpies["tasks:changed"] ?? []) {
      expect(unlisten).toHaveBeenCalled();
    }
  });

  it("shows the backend's own error when list_tasks rejects", async () => {
    mockListen.mockResolvedValue(vi.fn());
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") {
        throw { code: "internal", message: "the database is unavailable" };
      }
      if (command === "list_repositories") return [repository()];
      if (command === "get_run_environment") return "inherit";
      if (command === "get_queue_status") return queueStatus();
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunsView />);

    expect(await screen.findByText("the database is unavailable")).toBeInTheDocument();
  });

  // ---------------------------------------------------------------------------
  // The run queue (task 009; ADR-0010, ADR-0012).
  // ---------------------------------------------------------------------------

  describe("the run queue", () => {
    it("shows Reading queue status… before the first read resolves", async () => {
      mockListen.mockResolvedValue(vi.fn());
      let resolveStatus: ((status: QueueStatus) => void) | undefined;
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          return new Promise((resolve) => {
            resolveStatus = resolve;
          });
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);

      expect(screen.getByText("Reading queue status…")).toBeInTheDocument();

      resolveStatus?.(queueStatus());
      await waitFor(() => expect(screen.queryByText("Reading queue status…")).toBeNull());
    });

    it("shows Running when the queue's state is running", async () => {
      mockBackend({ queue: queueStatus({ state: "running" }) });
      render(<RunsView />);

      expect(await screen.findByText("Running")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Pause" })).toBeInTheDocument();
    });

    it("shows Paused, and a Start-queue control, before the queue has ever run", async () => {
      mockBackend({ queue: queueStatus({ state: "paused" }) });
      render(<RunsView />);

      expect(await screen.findByText("Paused")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Start queue" })).toBeInTheDocument();
    });

    it("shows the last pass's own failure next to the state badge", async () => {
      // Finding 5 of task 009's own verification report: a missing `claude`
      // fails `probe_cli` before any task is even chosen, so no `SkipReason`
      // on the plan below can say why — without this, the state badge above
      // reads "Running" over a full plan while nothing is actually happening.
      mockBackend({
        queue: queueStatus({
          state: "running",
          lastStepError: "the Claude Code CLI could not be found on PATH",
        }),
      });
      render(<RunsView />);

      expect(await screen.findByText("Running")).toBeInTheDocument();
      expect(
        screen.getByText(
          "The queue could not complete its last pass: the Claude Code CLI could not be found on PATH",
        ),
      ).toBeInTheDocument();
    });

    it("shows nothing extra once the last pass succeeded", async () => {
      mockBackend({ queue: queueStatus({ state: "running", lastStepError: null }) });
      render(<RunsView />);

      await screen.findByText("Running");
      expect(screen.queryByText(/could not complete its last pass/)).toBeNull();
    });

    it("keeps Stop offered through a paused queue with a run still in flight", async () => {
      // `runningTaskId` non-null while `state` is already `paused` is exactly
      // ADR-0010's "Pause lets the current run finish" — `QueueControls`
      // itself is unit-tested against this combination directly; this proves
      // `RunsView` actually wires `queueStatus.runningTaskId` into it rather
      // than, say, always passing `false`.
      mockBackend({ queue: queueStatus({ state: "paused", runningTaskId: "task-1" }) });
      render(<RunsView />);

      expect(await screen.findByText("Paused")).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Stop" })).toBeInTheDocument();
    });

    it("offers Resume, not Start, once the queue has been observed running and then pauses", async () => {
      let call = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          call += 1;
          // Running on the first read, paused from the second on.
          return queueStatus({ state: call === 1 ? "running" : "paused" });
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      expect(await screen.findByRole("button", { name: "Pause" })).toBeInTheDocument();

      act(() => fire("tasks:changed", ["task-1"]));

      expect(await screen.findByRole("button", { name: "Resume queue" })).toBeInTheDocument();
      expect(screen.queryByRole("button", { name: "Start queue" })).toBeNull();
    });

    it("shows the ordered plan from the queue's own status", async () => {
      mockBackend({
        queue: queueStatus({
          plan: [
            { taskId: "task-1", title: "First up", repositoryId: "repo-1", queuePosition: 1, skip: null },
            {
              taskId: "task-2",
              title: "Second, blocked",
              repositoryId: "repo-1",
              queuePosition: null,
              skip: "unattended_runs_not_allowed",
            },
          ],
        }),
      });
      render(<RunsView />);

      expect(await screen.findByText("First up")).toBeInTheDocument();
      expect(screen.getByText("#1")).toBeInTheDocument();
      expect(screen.getByText("Second, blocked")).toBeInTheDocument();
      expect(
        screen.getByText("Not queued — this repository has not enabled unattended agent runs"),
      ).toBeInTheDocument();
    });

    it("re-reads the queue status on runs:changed", async () => {
      let call = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          call += 1;
          return queueStatus({ state: call === 1 ? "paused" : "running" });
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(call).toBe(1));
      expect(screen.getByText("Paused")).toBeInTheDocument();

      act(() => fire("runs:changed", ["run-1"]));

      expect(await screen.findByText("Running")).toBeInTheDocument();
    });

    it("re-reads the queue status on settings:changed", async () => {
      let call = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          call += 1;
          return queueStatus({ state: call === 1 ? "paused" : "running" });
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(call).toBe(1));

      act(() => fire("settings:changed", null));

      expect(await screen.findByText("Running")).toBeInTheDocument();
    });

    it("shows the placeholder for completed-this-session before anything has finished", async () => {
      mockBackend({});
      render(<RunsView />);

      expect(await screen.findByText(/Nothing has finished yet/)).toBeInTheDocument();
    });

    it("records a session outcome once the queue's in-flight task changes away from it", async () => {
      let call = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command, args) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          call += 1;
          // First read: the queue is mid-run on task-1. Second read (after
          // `tasks:changed` fires below): task-1 is no longer in flight.
          return queueStatus({ state: "running", runningTaskId: call === 1 ? "task-1" : null });
        }
        if (command === "get_task") {
          const taskId = (args as { id: string }).id;
          return taskDetailFor(taskId, {
            title: "Add truncate_slug",
            repositoryId: "repo-1",
            lastRun: {
              id: "run-1",
              taskId,
              attempt: 1,
              status: "succeeded",
              sessionId: "session-1",
              prompt: "prompt",
              startedAt: "2026-08-20T11:00:00Z",
              endedAt: "2026-08-20T11:30:00Z",
              exitClass: "success",
              errorMessage: null,
              numTurns: 4,
              costUsd: 0.05,
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
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(call).toBe(1));

      act(() => fire("tasks:changed", ["task-1"]));

      expect(await screen.findByText("Add truncate_slug")).toBeInTheDocument();
      expect(screen.getByText("Succeeded")).toBeInTheDocument();
    });

    it("never records the same finished run twice across two events that both fire", async () => {
      let statusCall = 0;
      let getTaskCall = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command, args) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          statusCall += 1;
          return queueStatus({ state: "running", runningTaskId: statusCall === 1 ? "task-1" : null });
        }
        if (command === "get_task") {
          getTaskCall += 1;
          const taskId = (args as { id: string }).id;
          return taskDetailFor(taskId, {
            title: "Add truncate_slug",
            lastRun: {
              id: "run-1",
              taskId,
              attempt: 1,
              status: "succeeded",
              sessionId: "session-1",
              prompt: "prompt",
              startedAt: "2026-08-20T11:00:00Z",
              endedAt: "2026-08-20T11:30:00Z",
              exitClass: "success",
              errorMessage: null,
              numTurns: 4,
              costUsd: 0.05,
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
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(statusCall).toBe(1));

      // Both events fire for the same transition — a run's own `runs:changed`
      // and the board's `tasks:changed` — and both independently re-read
      // `get_queue_status`, which keeps reporting `runningTaskId: null` once
      // task-1 has finished. The second re-read must not re-resolve the
      // outcome: the transition (task-1 -> null) already happened once.
      act(() => fire("tasks:changed", ["task-1"]));
      await screen.findByText("Add truncate_slug");
      act(() => fire("runs:changed", ["run-1"]));

      await waitFor(() => expect(statusCall).toBeGreaterThanOrEqual(3));
      expect(screen.getAllByText("Add truncate_slug")).toHaveLength(1);
      expect(getTaskCall).toBe(1);
    });

    it("dedupes two different transitions whose get_task calls resolve to the same run out of order", async () => {
      // `get_task` always answers with *current* data, not a snapshot bound to
      // when it was dispatched — so two distinct, correctly-detected
      // transitions (task-1 finishing, then task-2 finishing) can still race
      // to the same underlying run id if the first call is slow enough to
      // resolve after the second. This is the scenario `resolvedRunIds`
      // guards against, distinct from "the same transition observed twice"
      // (covered above), which the transition-ref alone already prevents.
      let statusCall = 0;
      const { fire } = mockBackend({});
      const getTaskResolvers: Record<string, (detail: TaskDetail) => void> = {};
      mockInvoke.mockImplementation(async (command, args) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") {
          statusCall += 1;
          // task-1 running -> task-2 running -> nothing running.
          const runningTaskId =
            statusCall === 1 ? "task-1" : statusCall === 2 ? "task-2" : null;
          return queueStatus({ state: "running", runningTaskId });
        }
        if (command === "get_task") {
          const taskId = (args as { id: string }).id;
          return new Promise((resolve) => {
            getTaskResolvers[taskId] = resolve;
          });
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(statusCall).toBe(1));

      // Detects task-1 -> task-2, dispatches get_task("task-1").
      act(() => fire("tasks:changed", ["task-1"]));
      await waitFor(() => expect(getTaskResolvers["task-1"]).toBeDefined());

      // Detects task-2 -> null, dispatches get_task("task-2").
      act(() => fire("tasks:changed", ["task-2"]));
      await waitFor(() => expect(getTaskResolvers["task-2"]).toBeDefined());

      const sharedLastRun = {
        id: "run-shared",
        attempt: 1,
        status: "succeeded" as const,
        sessionId: "session-1",
        prompt: "prompt",
        startedAt: "2026-08-20T11:00:00Z",
        endedAt: "2026-08-20T11:30:00Z",
        exitClass: "success" as const,
        errorMessage: null,
        numTurns: 4,
        costUsd: 0.05,
        logPath: "/data/runs/run-shared.jsonl",
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
      };

      // Resolved out of dispatch order: task-2's call (dispatched second)
      // answers first, both naming the same run id.
      act(() =>
        getTaskResolvers["task-2"]?.(
          taskDetailFor("task-2", {
            title: "Second",
            lastRun: { ...sharedLastRun, taskId: "task-2" },
          }),
        ),
      );
      await screen.findByText("Second");

      act(() =>
        getTaskResolvers["task-1"]?.(
          taskDetailFor("task-1", {
            title: "First",
            lastRun: { ...sharedLastRun, taskId: "task-1" },
          }),
        ),
      );

      // Give the (deliberately unwanted) second entry a chance to land before
      // asserting it did not.
      await waitFor(() => expect(screen.getAllByRole("listitem").length).toBeGreaterThan(0));
      expect(screen.getAllByRole("listitem")).toHaveLength(1);
      expect(screen.queryByText("First")).toBeNull();
    });
  });

  // ---------------------------------------------------------------------------
  // Run history and filtering (task 015; ADR-0013).
  // ---------------------------------------------------------------------------

  describe("history", () => {
    it("lists every run list_runs reports, across every repository", async () => {
      mockBackend({
        historyEntries: [
          runListEntry({ id: "run-1", taskTitle: "First task" }),
          runListEntry({ id: "run-2", taskTitle: "Second task", exitClass: "fatal" }),
        ],
      });

      render(<RunsView />);

      expect(await screen.findByText("First task")).toBeInTheDocument();
      expect(screen.getByText("Second task")).toBeInTheDocument();
    });

    it("shows a message when no runs match the filters", async () => {
      mockBackend({ historyEntries: [] });

      render(<RunsView />);

      expect(await screen.findByText("No runs match these filters.")).toBeInTheDocument();
    });

    it("re-reads the history list on runs:changed", async () => {
      let call = 0;
      const { fire } = mockBackend({});
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") return [repository()];
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") return queueStatus();
        if (command === "list_runs") {
          call += 1;
          return call === 1 ? [] : [runListEntry({ taskTitle: "Freshly finished" })];
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await waitFor(() => expect(call).toBe(1));
      expect(screen.getByText("No runs match these filters.")).toBeInTheDocument();

      act(() => fire("runs:changed", ["run-1"]));

      expect(await screen.findByText("Freshly finished")).toBeInTheDocument();
    });

    it("sends the chosen repository as a filter to list_runs", async () => {
      mockListen.mockResolvedValue(vi.fn());
      let lastFilter: unknown;
      mockInvoke.mockImplementation(async (command, args) => {
        if (command === "list_tasks") return [];
        if (command === "list_repositories") {
          return [repository({ id: "repo-1", name: "rimaia" }), repository({ id: "repo-2", name: "other" })];
        }
        if (command === "get_run_environment") return "inherit";
        if (command === "get_queue_status") return queueStatus();
        if (command === "list_runs") {
          lastFilter = (args as { filter: unknown }).filter;
          return [];
        }
        throw new Error(`unexpected command: ${command}`);
      });

      render(<RunsView />);
      await screen.findByText("other");
      expect(lastFilter).toEqual({});

      fireEvent.change(screen.getByLabelText("Repository"), { target: { value: "repo-2" } });

      await waitFor(() => expect(lastFilter).toEqual({ repositoryId: "repo-2" }));
    });
  });
});
