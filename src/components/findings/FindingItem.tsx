import { useState } from "react";

import { findingStatusText } from "../../lib/review";
import type { FindingSeverity, HistoryFinding } from "../../types";

/** A severity as a word. Only labelled here, never compared: whether a finding
 *  blocks is `HistoryFinding.blocking`, decided in core against the effective
 *  `blocking_severity`. */
export const SEVERITY_LABELS: Record<FindingSeverity, string> = {
  critical: "Critical",
  high: "High",
  medium: "Medium",
  low: "Low",
};

/** Core's two ping-pong lists, as the marks a finding wears. */
export const CAME_BACK_TEXT = "Came back after a fix";
export const NEW_AFTER_FIX_TEXT = "New after a fix";

interface FindingItemProps {
  readonly finding: HistoryFinding;
  /** Run id to `attempt`, for `Fixed in #6`. */
  readonly attempts: ReadonlyMap<string, number>;
  readonly cameBack?: boolean;
  readonly newAfterFix?: boolean;
}

/**
 * One finding, in the same shape wherever the loop's findings are read: the
 * task panel's history, the run detail overlay and the morning review.
 *
 * Open, advisory, fixed and rejected differ by **words** and by the shape of
 * the rule down the left edge, never by colour alone (ADR-0024 rule 3). A
 * rejection's reason is always on the line, never behind the disclosure: a
 * finding declined without a reason reads in the morning exactly like one that
 * was ignored.
 */
export function FindingItem({ finding, attempts, cameBack, newAfterFix }: FindingItemProps) {
  const [copied, setCopied] = useState(false);
  const location = finding.file
    ? finding.line != null
      ? `${finding.file}:${finding.line}`
      : finding.file
    : null;
  const advisory = finding.status === "open" && !finding.blocking;

  async function copyLocation() {
    if (!location) return;
    try {
      await navigator.clipboard.writeText(location);
      setCopied(true);
    } catch {
      // The location is on screen as text; copying is a convenience on top.
      setCopied(false);
    }
  }

  return (
    <li
      className={`finding finding-${finding.status}${advisory ? " finding-advisory" : ""}`}
      data-finding-id={finding.id}
    >
      <div className="finding-head">
        <span className="finding-severity">{SEVERITY_LABELS[finding.severity]}</span>
        <span className="finding-title">{finding.title}</span>
      </div>
      <div className="finding-meta">
        <span className="finding-status">{findingStatusText(finding, attempts)}</span>
        {advisory && <span className="finding-mark">Advisory</span>}
        {cameBack && <span className="finding-mark finding-mark-signal">{CAME_BACK_TEXT}</span>}
        {newAfterFix && (
          <span className="finding-mark finding-mark-signal">{NEW_AFTER_FIX_TEXT}</span>
        )}
      </div>
      {location && (
        <button
          type="button"
          className="finding-location"
          onClick={copyLocation}
          title="Copy the location"
          aria-label={`Copy ${location}`}
        >
          <code>{location}</code>
          {copied && <span className="finding-copied">Copied</span>}
        </button>
      )}
      <details className="finding-details">
        <summary>Details</summary>
        <p className="finding-body">{finding.body}</p>
      </details>
    </li>
  );
}
