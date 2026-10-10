import { useCallback, useEffect, useState } from "react";

import { toRimaiaError } from "../lib/commands";
import type { UnlistenFn } from "../lib/events";
import type { RimaiaError } from "../types";

/** What every component reading one shared value sees. */
export interface SharedReadState<T> {
  /** `null` until the first read answers. */
  readonly value: T | null;
  readonly error: RimaiaError | null;
  /** Reads again now, for the component that just wrote: its own write's
   *  event re-reads too, but a caller should not have to wait on it. */
  readonly reload: () => void;
}

/**
 * One read shared by every mounted component that asks for it, re-read on one
 * event (task 066's two hooks).
 *
 * A board renders fifty cards and each needs this machine's checkouts and
 * worktree records; fifty `invoke`s per `tasks:changed` would be the N+1
 * seam-contract D12 argues against. So the first subscriber starts the read
 * and the subscription, every later one shares them, and the last one to
 * unmount tears both down, so a later mount starts again from a real read
 * rather than a stale snapshot. Only the newest read may commit.
 */
export function createSharedRead<T>(
  read: () => Promise<T>,
  subscribe: (onChanged: () => void) => Promise<UnlistenFn>,
): () => SharedReadState<T> {
  let state: { value: T | null; error: RimaiaError | null } = { value: null, error: null };
  let latest = 0;
  let unlisten: UnlistenFn | null = null;
  let subscribing = false;
  const listeners = new Set<(next: { value: T | null; error: RimaiaError | null }) => void>();

  function publish(next: { value: T | null; error: RimaiaError | null }) {
    state = next;
    for (const listener of listeners) listener(next);
  }

  function refresh() {
    const request = (latest += 1);
    read().then(
      (value) => {
        if (latest === request && listeners.size > 0) publish({ value, error: null });
      },
      (thrown) => {
        if (latest === request && listeners.size > 0) {
          publish({ value: state.value, error: toRimaiaError(thrown) });
        }
      },
    );
  }

  function start() {
    refresh();
    if (subscribing) return;
    subscribing = true;
    subscribe(() => {
      if (listeners.size > 0) refresh();
    }).then(
      (stop) => {
        if (listeners.size > 0) unlisten = stop;
        else stop();
        subscribing = false;
      },
      () => {
        // No event bridge (a non-Tauri preview): read once and stay as read.
        subscribing = false;
      },
    );
  }

  function stop() {
    unlisten?.();
    unlisten = null;
    latest += 1;
    state = { value: null, error: null };
  }

  return function useSharedRead(): SharedReadState<T> {
    const [current, setCurrent] = useState(state);
    const reload = useCallback(() => {
      if (listeners.size > 0) refresh();
    }, []);
    useEffect(() => {
      listeners.add(setCurrent);
      if (listeners.size === 1) start();
      else setCurrent(state);
      return () => {
        listeners.delete(setCurrent);
        if (listeners.size === 0) stop();
      };
    }, []);
    return { ...current, reload };
  };
}
