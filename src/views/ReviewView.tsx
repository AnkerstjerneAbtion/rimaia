import { useCallback, useState } from "react";

import { DigestPanel } from "../components/review/DigestPanel";
import { ReviewQueue } from "../components/review/ReviewQueue";
import { useReviewDigest } from "../hooks/useReviewDigest";
import { useTasks } from "../hooks/useTasks";
import { reviewQueue } from "../lib/review";
import type { ReviewMode } from "../lib/review";

interface ReviewViewProps {
  /** The clock, injected so a test can fix it (CLAUDE.md: fake the clock). */
  readonly now?: () => Date;
}

/**
 * The morning review (task 017): the overnight digest, and the queue of tasks
 * waiting for a verdict. It opens on the digest, and a review from the opened
 * digest to the empty queue needs no pointer.
 */
export function ReviewView({ now = () => new Date() }: ReviewViewProps) {
  const [mode, setMode] = useState<ReviewMode>("digest");
  const { digest, loading: digestLoading, error: digestError } = useReviewDigest();
  const { state, loading, readError, refresh } = useTasks(null);

  const showDigest = useCallback(() => setMode("digest"), []);
  const showQueue = useCallback(() => setMode("queue"), []);
  const queueCount = reviewQueue(state.tasks).length;
  const clock = now();

  return (
    <div className="review-view">
      <header className="review-view-head">
        <h2>Review</h2>
        <div className="review-modes" role="group" aria-label="Review mode">
          <button type="button" aria-pressed={mode === "digest"} onClick={showDigest}>
            Digest
          </button>
          <button type="button" aria-pressed={mode === "queue"} onClick={showQueue}>
            Queue <span className="tabular-nums">({queueCount})</span>
          </button>
        </div>
      </header>
      {mode === "digest" ? (
        <DigestPanel
          digest={digest}
          loading={digestLoading}
          error={digestError}
          queueCount={queueCount}
          now={clock}
          onStartReview={showQueue}
        />
      ) : (
        <ReviewQueue
          tasks={state.tasks}
          loading={loading}
          readError={readError}
          refresh={refresh}
          now={clock}
          onShowDigest={showDigest}
        />
      )}
    </div>
  );
}
