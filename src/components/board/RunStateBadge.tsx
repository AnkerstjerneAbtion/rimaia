import { cardBadge, formatResumeAfter } from "../../lib/board";
import type { ExitClass, RunState } from "../../types";

const LABELS: Record<NonNullable<ReturnType<typeof cardBadge>>, string> = {
  running: "Running",
  queued: "Queued",
  blocked: "Blocked",
  waiting_retry: "Waiting for retry",
  failed: "Failed",
  interrupted: "Interrupted",
  cancelled: "Cancelled",
};

interface RunStateBadgeProps {
  runState: RunState;
  /** Structurally typed rather than `LastRunSummary`, so both the summary the
   *  card holds and the whole `Run` the panel holds satisfy it. */
  lastRun: {
    readonly exitClass: ExitClass | null;
    readonly resumeAfter?: string | null;
  } | null;
}

/**
 * The only thing on a card that renders `runState`/`lastRun` directly —
 * everything about *which* badge that is comes from `cardBadge` (task 005:
 * "visually distinct and unambiguous", D9's interrupted-vs-failed word).
 * `idle` renders nothing, matching `cardBadge`'s own null case.
 */
export function RunStateBadge({ runState, lastRun }: RunStateBadgeProps) {
  const badge = cardBadge(runState, lastRun);
  if (badge === null) return null;

  // Task 014's "card badge showing `waiting_retry` **with the time it will
  // resume**". The time is the whole value of the badge at 09:00: without it a
  // card that is coming back at 06:12 and one whose retries ran out both read
  // as a bare "Waiting for retry", and only one of them needs a human.
  const resumeAt =
    badge === "waiting_retry" ? formatResumeAfter(lastRun?.resumeAfter ?? null) : null;

  return (
    <span className={`run-badge run-badge-${badge}`}>
      {LABELS[badge]}
      {resumeAt && <span className="run-badge-detail"> · {resumeAt}</span>}
    </span>
  );
}
