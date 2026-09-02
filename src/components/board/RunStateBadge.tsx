import { cardBadge } from "../../lib/board";
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
  lastRun: { readonly exitClass: ExitClass | null } | null;
  /** ADR-0008, off `TaskSummary.blockedByIncomplete`. Optional because a bare
   *  `Task` has no such field — see `CardTask` in `TaskCard.tsx` — and a card
   *  built from one has nothing true to say about blocking. */
  blockedByIncomplete?: boolean;
}

/**
 * The only thing on a card that renders `runState`/`lastRun` directly —
 * everything about *which* badge that is comes from `cardBadge` (task 005:
 * "visually distinct and unambiguous", D9's interrupted-vs-failed word,
 * ADR-0008's blocked). `idle` with nothing blocking it renders nothing,
 * matching `cardBadge`'s own null case.
 */
export function RunStateBadge({ runState, lastRun, blockedByIncomplete }: RunStateBadgeProps) {
  const badge = cardBadge(runState, lastRun, blockedByIncomplete ?? false);
  if (badge === null) return null;

  return <span className={`run-badge run-badge-${badge}`}>{LABELS[badge]}</span>;
}
