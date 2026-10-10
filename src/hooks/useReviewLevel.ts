import { useCallback, useEffect, useRef, useState } from "react";

import { getReviewLevel, toRimaiaError } from "../lib/commands";
import {
  subscribeToRepositoriesChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "../lib/events";
import type { ReviewLevel, ReviewLevelName, RimaiaError } from "../types";

export interface UseReviewLevelResult {
  /** `null` until the first read resolves. */
  readonly data: ReviewLevel | null;
  readonly error: RimaiaError | null;
  readonly dismissError: () => void;
  /** Reads the level again; a writer awaits this so the form shows what the
   *  backend kept, not what was asked for. */
  readonly reload: () => Promise<void>;
}

/**
 * One level of the review loop's configuration, next to what it inherits and
 * what it resolves to (task 037).
 *
 * The stored value wins over the form: another window, or an MCP client, is a
 * supported writer of the same rows (ADR-0006). So it re-reads on everything
 * that can move any of the three documents the answer is made of: the global
 * settings (`settings:changed`), a repository's (`repositories:changed`) and a
 * task's (`tasks:changed`). An empty id array is the wholesale-re-read signal.
 */
export function useReviewLevel(
  level: ReviewLevelName,
  id: string | null,
): UseReviewLevelResult {
  const [data, setData] = useState<ReviewLevel | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const reloadRef = useRef<() => Promise<void>>(() => Promise.resolve());
  const reload = useCallback(() => reloadRef.current(), []);

  useEffect(() => {
    let active = true;
    setData(null);
    if (level !== "global" && id === null) return;

    function load(): Promise<void> {
      return getReviewLevel(level, id ?? undefined).then(
        (result) => {
          if (active) {
            setData(result);
            setError(null);
          }
        },
        (thrown) => {
          if (active) setError(toRimaiaError(thrown));
        },
      );
    }
    reloadRef.current = load;
    void load();

    const unlisteners: Array<() => void> = [];
    function watch(subscribed: Promise<() => void>) {
      subscribed.then(
        (unlisten) => {
          if (active) unlisteners.push(unlisten);
          else unlisten();
        },
        () => {
          // No event bridge (tests, or a non-Tauri preview): the first read
          // and the writer's own reload are all this will ever show.
        },
      );
    }
    watch(subscribeToSettingsChanged(() => active && void load()));
    watch(subscribeToRepositoriesChanged(() => active && void load()));
    watch(
      subscribeToTasksChanged((ids) => {
        if (!active) return;
        if (level === "task" && ids.length !== 0 && id !== null && !ids.includes(id)) return;
        void load();
      }),
    );

    return () => {
      active = false;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [level, id]);

  return { data, error, dismissError: () => setError(null), reload };
}
