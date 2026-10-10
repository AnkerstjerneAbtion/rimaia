import { useCallback, useEffect, useRef, useState } from "react";

import { getRun, getTask, getTaskDependents, toRimaiaError } from "../lib/commands";
import { subscribeToRunsChanged, subscribeToTasksChanged } from "../lib/events";
import type { RimaiaError, RunDetail, TaskDependent, TaskDetail } from "../types";

export interface ReviewTarget {
  /** The task as `get_task` reads it: its newest run and its own dependencies. */
  readonly task: TaskDetail;
  /** The newest run's `get_run`, or `null` when the task has no run. */
  readonly run: RunDetail | null;
  /** Direct dependents, from task 034's `dependents_of`. */
  readonly dependents: TaskDependent[];
}

export interface UseReviewTargetResult {
  /** `null` until the first read for this task has landed. */
  readonly target: ReviewTarget | null;
  readonly error: RimaiaError | null;
}

/**
 * What the review view reads for the task on screen (task 017): `get_task` for
 * the newest run, then `get_run` for that run's recorded review, and
 * `get_task_dependents` beside them. `TaskSummary` carries no run id, so the
 * board's own read cannot stand in for the first.
 *
 * Re-read when a `tasks:changed` or `runs:changed` payload names the task, or
 * carries no ids — an empty payload means "every id changed" (seam-contract
 * D7), so it must refresh rather than be skipped.
 */
export function useReviewTarget(taskId: string | null): UseReviewTargetResult {
  const [target, setTarget] = useState<ReviewTarget | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const latestRequest = useRef(0);

  const read = useCallback((id: string) => {
    const request = (latestRequest.current += 1);
    const dependents = getTaskDependents(id);
    getTask(id)
      .then(async (task) => {
        const run = task.lastRun ? await getRun(task.lastRun.id) : null;
        return { task, run, dependents: await dependents };
      })
      .then(
        (next) => {
          if (latestRequest.current !== request) return;
          setTarget(next);
          setError(null);
        },
        (thrown) => {
          if (latestRequest.current !== request) return;
          setError(toRimaiaError(thrown));
        },
      );
  }, []);

  useEffect(() => {
    // The previous task's review must not sit under the next task's title
    // while its own reads are in flight.
    setTarget(null);
    setError(null);
    if (taskId === null) {
      latestRequest.current += 1;
      return;
    }
    read(taskId);
  }, [taskId, read]);

  useEffect(() => {
    if (taskId === null) return;
    let active = true;
    const unlisteners: Array<() => void> = [];
    const names = (ids: string[]) => ids.length === 0 || ids.includes(taskId);
    const track = (subscription: Promise<() => void>) =>
      subscription.then(
        (unlisten) => {
          if (active) unlisteners.push(unlisten);
          else unlisten();
        },
        () => {},
      );
    // `runs:changed` carries run ids, which say nothing about which task they
    // belong to, so any run event re-reads the one on screen.
    void track(
      subscribeToTasksChanged((ids) => {
        if (active && names(ids)) read(taskId);
      }),
    );
    void track(
      subscribeToRunsChanged(() => {
        if (active) read(taskId);
      }),
    );
    return () => {
      active = false;
      for (const unlisten of unlisteners) unlisten();
    };
  }, [taskId, read]);

  return {
    target: target !== null && target.task.id === taskId ? target : null,
    error,
  };
}
