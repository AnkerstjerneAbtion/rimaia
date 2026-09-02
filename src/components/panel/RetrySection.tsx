import { useState } from "react";

import { formatResumeAfter } from "../../lib/board";
import { giveUpOnTask, retryTaskNow, toRimaiaError } from "../../lib/commands";
import type { RimaiaError, Run, RunState } from "../../types";

interface RetrySectionProps {
  readonly taskId: string;
  readonly runState: RunState;
  readonly lastRun: Run | null;
  readonly loading: boolean;
  /** Re-reads the panel's `TaskDetail`, so the badge above updates with it. */
  readonly onChanged: () => void;
}

/**
 * Task 014's manual pair: "Retry now" and "Give up", for a task waiting out
 * ADR-0011's backoff.
 *
 * # It renders nothing unless there is a wait to act on
 *
 * A section that was always visible would put two run-control buttons under
 * every card, on a panel whose "Run now" already lives on the card itself. The
 * two verbs here only mean anything for a task in `waiting_retry`: "retry now"
 * on a running task is nothing, and "give up" on one is Cancel.
 *
 * # Why the deadline is repeated here
 *
 * The card badge already shows it, and this is the panel — the two are almost
 * never on screen together, and a button whose whole point is "sooner than
 * that" is unusable without a *that*. This is also the only place the operator
 * sees the wait beside the error that caused it (`RunOutcomeSection` is
 * immediately above), which is the pairing that makes "give up" a decision
 * rather than a guess.
 */
export function RetrySection({
  taskId,
  runState,
  lastRun,
  loading,
  onChanged,
}: RetrySectionProps) {
  const [busy, setBusy] = useState<"retry" | "give-up" | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  if (loading || runState !== "waiting_retry") return null;

  const resumeAt = formatResumeAfter(lastRun?.resumeAfter ?? null);

  function act(kind: "retry" | "give-up") {
    setBusy(kind);
    setError(null);
    const request = kind === "retry" ? retryTaskNow(taskId) : giveUpOnTask(taskId);
    request.then(
      () => {
        setBusy(null);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setBusy(null);
      },
    );
  }

  return (
    <section className="task-detail-section retry-section">
      <h4>Waiting to resume</h4>
      <p className="muted">
        {resumeAt === null
          ? "This task is waiting, but nothing is scheduled for it."
          : resumeAt === "resuming"
            ? "Its deadline has passed — the queue picks it up as soon as it is running."
            : `The queue will ${resumeAt}. Retrying now does not wait for that.`}
      </p>
      <div className="retry-section-actions">
        <button type="button" disabled={busy !== null} onClick={() => act("retry")}>
          {busy === "retry" ? "Starting…" : "Retry now"}
        </button>
        <button type="button" disabled={busy !== null} onClick={() => act("give-up")}>
          {busy === "give-up" ? "Giving up…" : "Give up"}
        </button>
      </div>
      {error && <p className="retry-section-error">{error.message}</p>}
    </section>
  );
}
