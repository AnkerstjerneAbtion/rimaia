import { useReviewHistory } from "../../hooks/useReviewHistory";
import { runLabel } from "../../lib/board";
import { attemptsByRun, hasReviewHistory, reviewHistoryHeadText } from "../../lib/review";
import type { LoopHistory, PhaseSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { FindingItem } from "../findings/FindingItem";
import { EXIT_CLASS_LABELS } from "./RunOutcomeSection";

interface ReviewHistorySectionProps {
  readonly taskId: string;
}

/**
 * What the review loop found on this task, loop by loop (task 037, ADR-0017).
 *
 * Rendered as `get_review_history` returned it. Grouping runs into loops, what
 * blocks, the counts in the head and the two ping-pong lists are core's
 * (task 021); this component has nothing to decide, and a TypeScript copy of
 * any of those rules would be free to disagree with the one the loop obeys.
 *
 * Renders nothing until the task has a review or a fix to show, so a task the
 * loop never touched keeps the panel it always had.
 *
 * It reloads on everything that can change what the history says: a finding
 * recorded or resolved (`tasks:changed`, which `record` and `resolve` publish),
 * a run starting or ending (`runs:changed`), and the settings and repository
 * writes that move `blocking_severity`.
 */
export function ReviewHistorySection({ taskId }: ReviewHistorySectionProps) {
  const { history, error, dismissError } = useReviewHistory(taskId);

  if (error) {
    return (
      <section className="task-detail-section review-history-section">
        <h4>Review history</h4>
        <ErrorBanner error={error} onDismiss={dismissError} />
      </section>
    );
  }
  if (!hasReviewHistory(history)) return null;

  const attempts = attemptsByRun(history);
  const current = history.loops.find((loop) => !loop.earlier);
  const head = current ? reviewHistoryHeadText(current) : null;

  return (
    <section className="task-detail-section review-history-section">
      <h4>Review history</h4>
      {head && <p className="review-history-head">{head}</p>}
      {history.loops.map((loop) =>
        loop.rounds.length === 0 ? null : loop.earlier ? (
          // Collapsed: an earlier loop belongs to work a re-run replaced, and
          // the morning is about the loop the branch came out of.
          <details key={loop.implementation.runIds[0]} className="review-history-earlier">
            <summary>Earlier loop · {phaseLabel(loop.implementation)}</summary>
            <LoopBody loop={loop} attempts={attempts} />
          </details>
        ) : (
          <LoopBody key={loop.implementation.runIds[0]} loop={loop} attempts={attempts} />
        ),
      )}
    </section>
  );
}

/** `Review · #4`, or `Review · #4, #5` for a phase a usage limit made resume. */
function phaseLabel(phase: PhaseSummary): string {
  const [first, ...rest] = phase.attempts;
  const label = runLabel(phase.kind, first);
  return rest.length === 0 ? label : `${label}, ${rest.map((attempt) => `#${attempt}`).join(", ")}`;
}

function phaseOutcome(phase: PhaseSummary): string {
  return phase.exitClass ? EXIT_CLASS_LABELS[phase.exitClass] : "Running";
}

function LoopBody({
  loop,
  attempts,
}: {
  readonly loop: LoopHistory;
  readonly attempts: ReadonlyMap<string, number>;
}) {
  return (
    <div className="review-loop">
      {loop.rounds.map((round, index) => {
        const regressed = new Set(round.regressed.map((finding) => finding.id));
        const newAfterFix = new Set(round.newAfterFix.map((finding) => finding.id));
        return (
          <div key={index} className="review-round">
            {round.review && (
              <>
                <h5 className="review-round-title">
                  <span>{phaseLabel(round.review)}</span>
                  <span className="muted">{phaseOutcome(round.review)}</span>
                </h5>
                {round.findings.length === 0 ? (
                  <p className="muted">Raised nothing.</p>
                ) : (
                  <ul className="finding-list">
                    {round.findings.map((finding) => (
                      <FindingItem
                        key={finding.id}
                        finding={finding}
                        attempts={attempts}
                        cameBack={regressed.has(finding.id)}
                        newAfterFix={newAfterFix.has(finding.id)}
                      />
                    ))}
                  </ul>
                )}
              </>
            )}
            {round.fix && (
              <h5 className="review-round-title">
                <span>{phaseLabel(round.fix.phase)}</span>
                <span className="muted">{phaseOutcome(round.fix.phase)}</span>
              </h5>
            )}
          </div>
        );
      })}
    </div>
  );
}
