import { badgeLabel, cardBadge, formatResumeAfter } from "../../lib/board";
import type { ExitClass, RunKind, RunState } from "../../types";

interface RunStateBadgeProps {
  runState: RunState;
  /** Structurally typed rather than `LastRunSummary`, so both the summary the
   *  card holds and the whole `Run` the panel holds satisfy it. */
  lastRun: {
    /** What the newest row was for: the words say "Reviewing" for a review
     *  and "Running" for an implementation, off the same state key. */
    readonly kind: RunKind;
    readonly exitClass: ExitClass | null;
    readonly resumeAfter?: string | null;
  } | null;
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

  // Task 014's "card badge showing `waiting_retry` **with the time it will
  // resume**". The time is the whole value of the badge at 09:00: without it a
  // card that is coming back at 06:12 and one whose retries ran out both read
  // as a bare "Waiting for retry", and only one of them needs a human.
  const resumeAt =
    badge === "waiting_retry" ? formatResumeAfter(lastRun?.resumeAfter ?? null) : null;

  return (
    <span className={`run-badge run-badge-${badge}`}>
      {/* THE DIRECTION's `.status-dot` vocabulary rather than a badge-specific
          glyph: one dot, coloured through `--badge-tone`, and only the running
          one moves. `aria-hidden` because the word beside it already says
          everything the dot does — the dot is the fast channel, never the
          only one (which is also what keeps the set readable in greyscale and
          to a colour-blind reader).

          The label stays a direct text child of this element rather than being
          wrapped: `.run-badge` is what carries the state class, and the tests
          find the badge by its own text. */}
      <span
        className={badge === "running" ? "status-dot status-dot-live" : "status-dot"}
        aria-hidden="true"
      />
      {badgeLabel(badge, lastRun?.kind)}
      {resumeAt && <span className="run-badge-detail"> · {resumeAt}</span>}
    </span>
  );
}
