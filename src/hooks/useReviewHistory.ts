import { useEffect, useState } from "react";

import { getReviewHistory, toRimaiaError } from "../lib/commands";
import {
  subscribeToRepositoriesChanged,
  subscribeToRunsChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "../lib/events";
import type { ReviewHistory, RimaiaError } from "../types";

export interface UseReviewHistoryResult {
  /** `null` until the first read resolves, and after one that failed. */
  readonly history: ReviewHistory | null;
  readonly error: RimaiaError | null;
  readonly dismissError: () => void;
}

/**
 * One task's review history, kept current (task 037).
 *
 * It reloads on everything that can change what the history says: a finding
 * recorded or resolved (`tasks:changed`, which `record` and `resolve` publish),
 * a run starting or ending (`runs:changed`), and the settings and repository
 * writes that move `blocking_severity` and so which findings block. An empty id
 * array is the wholesale-re-read signal (`events.ts`), so it reloads too.
 *
 * The history is whatever core returned. This hook groups, ranks and counts
 * nothing.
 */
export function useReviewHistory(taskId: string | null): UseReviewHistoryResult {
  const [history, setHistory] = useState<ReviewHistory | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    let active = true;
    setHistory(null);
    // A caller that does not know its task yet (an overlay still loading the
    // run) reads nothing rather than asking for a task called "".
    if (taskId === null) return;
    const id = taskId;

    function load() {
      getReviewHistory(id).then(
        (result) => {
          if (active) {
            setHistory(result);
            setError(null);
          }
        },
        (thrown) => {
          if (active) setError(toRimaiaError(thrown));
        },
      );
    }

    load();

    const unlisteners: Array<() => void> = [];
    function watch(subscribed: Promise<() => void>) {
      subscribed.then(
        (unlisten) => {
          if (active) unlisteners.push(unlisten);
          else unlisten();
        },
        () => {
          // No event bridge (tests, or a non-Tauri preview): the first read is
          // all this will ever show.
        },
      );
    }
    watch(
      subscribeToTasksChanged((ids) => {
        if (active && (ids.length === 0 || ids.includes(id))) load();
      }),
    );
    watch(
      subscribeToRunsChanged(() => {
        if (active) load();
      }),
    );
    watch(
      subscribeToSettingsChanged(() => {
        if (active) load();
      }),
    );
    watch(
      subscribeToRepositoriesChanged(() => {
        if (active) load();
      }),
    );

    return () => {
      active = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [taskId]);

  return { history, error, dismissError: () => setError(null) };
}
