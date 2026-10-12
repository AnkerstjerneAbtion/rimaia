import { attemptsByRun, findingsForRun } from "../../lib/review";
import type { ReviewHistory } from "../../types";
import { FindingItem } from "./FindingItem";

interface OpenFindingsListProps {
  /** `get_review_history`'s answer, or `null` while it is loading or failed. */
  readonly history: ReviewHistory | null;
  /** The run being read; left out in the morning review, which is always about
   *  the task's newest state. */
  readonly runId?: string;
}

/**
 * What the reviewer could not fix, beside the diff it was found in (ADR-0017's
 * "what the morning sees").
 *
 * For the task's newest row this is the current loop's open findings, blocking
 * before advisory by core's flag. For an older review row it is what that
 * review raised, each finding with its status today. Both come from
 * `get_review_history`; nothing here counts, groups or ranks anything.
 *
 * Renders nothing when the task has no review to speak of, so a task that never
 * ran the loop shows exactly what it showed before.
 */
export function OpenFindingsList({ history, runId }: OpenFindingsListProps) {
  if (!history) return null;
  const shown = findingsForRun(history, runId ?? null);
  if (!shown) return null;

  const attempts = attemptsByRun(history);
  const heading = shown.kind === "open" ? "Unresolved findings" : "Findings from this review";

  return (
    <section className="run-detail-section run-detail-findings" aria-label={heading}>
      <h4>{heading}</h4>
      {shown.findings.length === 0 ? (
        <p className="muted">
          {shown.kind === "open"
            ? "Nothing left open from the last review."
            : "This review raised nothing."}
        </p>
      ) : (
        <ul className="finding-list">
          {shown.findings.map((finding) => (
            <FindingItem key={finding.id} finding={finding} attempts={attempts} />
          ))}
        </ul>
      )}
    </section>
  );
}
