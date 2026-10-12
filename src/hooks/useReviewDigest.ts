import { useCallback, useEffect, useRef, useState } from "react";

import { getReviewDigest, toRimaiaError } from "../lib/commands";
import {
  subscribeToRunsChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "../lib/events";
import type { RimaiaError, ReviewDigest } from "../types";

export interface UseReviewDigestResult {
  readonly digest: ReviewDigest | null;
  readonly loading: boolean;
  readonly error: RimaiaError | null;
}

/**
 * The overnight digest, re-read when it could have changed (task 017): a run
 * ended (`runs:changed`), a Blocked or Skipped entry's column moved
 * (`tasks:changed`), or the marker moved (`settings:changed` — task 034's
 * `mark_review_digest_seen` write publishes it). Every payload means "re-read":
 * the digest is one service answer, there is nothing to reconcile an id
 * against, and an empty array is the lagged forwarder's "everything changed"
 * (seam-contract D7).
 */
export function useReviewDigest(): UseReviewDigestResult {
  const [digest, setDigest] = useState<ReviewDigest | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<RimaiaError | null>(null);

  // Only the newest read may commit; two events in quick succession otherwise
  // let the older answer land last.
  const latestRequest = useRef(0);

  const refresh = useCallback(() => {
    const request = (latestRequest.current += 1);
    getReviewDigest().then(
      (next) => {
        if (latestRequest.current !== request) return;
        setDigest(next);
        setError(null);
        setLoading(false);
      },
      (thrown) => {
        if (latestRequest.current !== request) return;
        setError(toRimaiaError(thrown));
        setLoading(false);
      },
    );
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    let active = true;
    const unlisteners: Array<() => void> = [];
    const track = (subscription: Promise<() => void>) =>
      subscription.then(
        (unlisten) => {
          if (active) unlisteners.push(unlisten);
          else unlisten();
        },
        () => {
          // No event bridge (a non-Tauri preview): the digest is read on mount
          // and stays as read.
        },
      );
    void track(subscribeToRunsChanged(() => active && refresh()));
    void track(subscribeToTasksChanged(() => active && refresh()));
    void track(subscribeToSettingsChanged(() => active && refresh()));
    return () => {
      active = false;
      for (const unlisten of unlisteners) unlisten();
    };
  }, [refresh]);

  return { digest, loading, error };
}
