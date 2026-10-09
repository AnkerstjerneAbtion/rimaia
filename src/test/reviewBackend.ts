import { vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  DetectedOpenInTarget,
  ReviewDigest,
  ReviewHistory,
  RunDetail,
  TaskDependent,
  TaskDetail,
  TaskSummary,
} from "../types";
import {
  emptyDigest,
  repository,
  runDetail,
  taskDetail,
} from "./reviewFixtures";

/**
 * A scripted backend for the review tests, installed at the Tauri seam (the
 * pattern `StorageSection.test.tsx` explains): `invoke` and `listen` are mocked,
 * never the wrappers, so each test asserts the exact command name and arguments
 * a keypress sent.
 *
 * The three verdicts really move the task, as the services do, so the queue a
 * later `list_tasks` returns is the queue the backend would hold.
 */
export interface ReviewBackend {
  tasks: TaskSummary[];
  digest: ReviewDigest;
  /** Per task: what `get_task` and its newest run's `get_run` answer. */
  details: Record<string, Partial<TaskDetail>>;
  runs: Record<string, RunDetail>;
  /** Per task: what `get_review_history` answers; no entry is no loops. */
  histories: Record<string, ReviewHistory>;
  dependents: Record<string, TaskDependent[]>;
  /** A command name to what it rejects with. */
  refusals: Record<string, unknown>;
  /** What `reject_task` answers as the set-aside branch. */
  setAsideBranch: string | null;
  openTargets: DetectedOpenInTarget[];
  /** Every command the view sent, with its arguments, in order. */
  calls: Array<[string, unknown]>;
  fire: (event: string, payload?: unknown) => void;
}

const WRITES = ["approve_task", "reject_task", "request_task_changes", "mark_review_digest_seen"];

export function installBackend(tasks: TaskSummary[]): ReviewBackend {
  const handlers: Record<string, Array<(event: { payload: unknown }) => void>> = {};
  vi.mocked(listen).mockImplementation(async (name, callback) => {
    (handlers[name as string] ??= []).push(callback as (event: { payload: unknown }) => void);
    return () => {};
  });

  const backend: ReviewBackend = {
    tasks,
    digest: emptyDigest(),
    details: {},
    runs: {},
    histories: {},
    dependents: {},
    refusals: {},
    setAsideBranch: "rimaia/task-1-2",
    openTargets: [{ target: "vs_code", label: "VS Code" }],
    calls: [],
    fire(event, payload = []) {
      for (const handler of handlers[event] ?? []) handler({ payload });
    },
  };

  vi.mocked(invoke).mockImplementation(async (command, args) => {
    backend.calls.push([command, args]);
    if (command in backend.refusals) throw backend.refusals[command];
    const id = (args as { id?: string; taskId?: string } | undefined);
    const taskId = id?.taskId ?? id?.id;
    switch (command) {
      case "list_tasks":
        return backend.tasks.filter((task) => task.archivedAt === null);
      case "list_repositories":
        return [repository()];
      case "get_review_digest":
        return backend.digest;
      case "get_task":
        return taskDetail(taskId as string, backend.details[taskId as string] ?? {});
      case "get_run": {
        const runId = (args as { runId: string }).runId;
        return backend.runs[runId] ?? runDetail(runId.replace("run-for-", ""));
      }
      case "get_review_history":
        return backend.histories[taskId as string] ?? { loops: [] };
      case "get_task_dependents":
        return backend.dependents[taskId as string] ?? [];
      case "list_open_in_targets":
        return backend.openTargets;
      case "approve_task":
      case "reject_task":
      case "request_task_changes": {
        const task = backend.tasks.find((candidate) => candidate.id === taskId) as TaskSummary;
        task.column = command === "approve_task" ? "done" : "ready";
        return command === "approve_task"
          ? task
          : { task, dependents: [], setAsideBranch: command === "reject_task" ? backend.setAsideBranch : null };
      }
      case "plugin:opener|open_url":
      case "open_task_worktree_in":
        return undefined;
      default:
        throw new Error(`unexpected command: ${command}`);
    }
  });
  return backend;
}

/** The commands that wrote, in the order they were sent. */
export function writesSent(backend: ReviewBackend): Array<[string, unknown]> {
  return backend.calls.filter(([command]) => WRITES.includes(command));
}

export function sent(backend: ReviewBackend, command: string): unknown[] {
  return backend.calls.filter(([name]) => name === command).map(([, args]) => args);
}
