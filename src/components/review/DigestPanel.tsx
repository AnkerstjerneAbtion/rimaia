import { useEffect, useRef, useState } from "react";

import {
  DIGEST_OUTCOME_LABELS,
  digestAsText,
  entryFigures,
  entryReason,
  formatDigestCost,
  formatSeconds,
  outcomeCounts,
  reviewCommandForKey,
} from "../../lib/review";
import { isActivatableTarget, isEditableTarget } from "../../lib/keyboard";
import { relativeTime } from "../../lib/board";
import type { DigestEntry, RimaiaError, ReviewDigest } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { KeyLegend } from "./KeyLegend";

interface DigestPanelProps {
  readonly digest: ReviewDigest | null;
  readonly loading: boolean;
  readonly error: RimaiaError | null;
  /** How many tasks are waiting in `in_review`, for the way into the queue. */
  readonly queueCount: number;
  readonly now: Date;
  readonly onStartReview: () => void;
}

/**
 * What the night did (task 017), rendered as task 034's `get_review_digest`
 * returned it: its order, its entries, its totals. Nothing here sorts,
 * buckets or adds — a rule enforced only in this component would be a rule the
 * MCP tool does not share (ADR-0006).
 */
export function DigestPanel({
  digest,
  loading,
  error,
  queueCount,
  now,
  onStartReview,
}: DigestPanelProps) {
  const [copied, setCopied] = useState(false);
  const surface = useRef<HTMLElement | null>(null);

  // Arriving here takes focus, so Enter works at once: left on the sidebar
  // button that brought the reader here, it would be a button's Enter and
  // start nothing.
  useEffect(() => {
    surface.current?.focus();
  }, []);

  // Enter starts the review, and is left alone while focus is on a control
  // that answers it natively: the copy button must still copy.
  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented) return;
      if (isEditableTarget(event.target) || isActivatableTarget(event.target)) return;
      if (reviewCommandForKey("digest", event) !== "start_review") return;
      event.preventDefault();
      onStartReview();
    }
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onStartReview]);

  async function copySummary() {
    if (!digest) return;
    try {
      await navigator.clipboard.writeText(digestAsText(digest, now));
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return (
    <section ref={surface} tabIndex={-1} className="review-digest" aria-label="Overnight digest">
      <div className="review-digest-head">
        <div>
          <h3>Overnight digest</h3>
          {digest && (
            <p className="muted review-digest-since">
              Runs that ended since {relativeTime(digest.since, now)}
            </p>
          )}
        </div>
        <div className="review-digest-actions">
          <button type="button" onClick={() => void copySummary()} disabled={!digest}>
            {copied ? "Copied" : "Copy summary"}
          </button>
          <button type="button" className="btn-primary" onClick={onStartReview}>
            {queueCount === 0 ? "Open the queue" : `Start the review (${queueCount})`}
          </button>
        </div>
      </div>

      {error && <ErrorBanner error={error} />}
      {loading && !digest && !error && <p className="muted">Reading the night…</p>}

      {digest && <DigestBody digest={digest} />}

      <KeyLegend mode="digest" />
    </section>
  );
}

function DigestBody({ digest }: { digest: ReviewDigest }) {
  const { totals } = digest;
  if (digest.entries.length === 0 && totals.runs === 0) {
    return <p className="review-digest-empty">Nothing has ended since the last finished review.</p>;
  }
  // The service sums only the runs that recorded a cost, so when none did, its
  // zero is a figure nobody measured (seam-contract D18).
  const costUnrecorded = totals.runs > 0 && totals.runsWithoutCost === totals.runs;
  return (
    <>
      <dl className="review-totals">
        <div>
          <dt>Runs</dt>
          <dd className="tabular-nums">{totals.runs}</dd>
        </div>
        <div>
          <dt>Run time</dt>
          <dd className="tabular-nums">{formatSeconds(totals.runSeconds)}</dd>
        </div>
        <div>
          <dt>Cost</dt>
          <dd className="tabular-nums">
            {costUnrecorded ? (
              <span className="muted">not recorded</span>
            ) : (
              formatDigestCost(totals.costUsd)
            )}
          </dd>
        </div>
        {totals.spanSeconds !== null && (
          <div>
            <dt>Wall-clock span</dt>
            <dd className="tabular-nums">{formatSeconds(totals.spanSeconds)}</dd>
          </div>
        )}
      </dl>
      {totals.runsWithoutCost > 0 && !costUnrecorded && (
        <p className="muted review-totals-note">
          {totals.runsWithoutCost} {totals.runsWithoutCost === 1 ? "run" : "runs"} had no
          recorded cost.
        </p>
      )}
      <p className="review-counts">
        {outcomeCounts(digest).map(({ outcome, count }) => (
          <span key={outcome} className={`review-count review-outcome-${outcome}`}>
            <span className="status-dot" aria-hidden="true" />
            {count} {DIGEST_OUTCOME_LABELS[outcome].toLowerCase()}
          </span>
        ))}
      </p>
      <ol className="review-entries">
        {digest.entries.map((entry) => (
          <DigestEntryRow key={entry.taskId} entry={entry} />
        ))}
      </ol>
    </>
  );
}

/** One task's night. Takes the entry whole, so a field task 035 adds to it
 *  (the review loop's summary) renders by being given, not by a rewrite. */
function DigestEntryRow({ entry }: { entry: DigestEntry }) {
  const reason = entryReason(entry);
  const figures = entryFigures(entry);
  const showError = entry.errorMessage !== null && entry.outcome !== "completed";
  return (
    <li className={`review-entry review-outcome-${entry.outcome}`}>
      <span className="review-entry-outcome">
        <span
          className={entry.outcome === "running" ? "status-dot status-dot-live" : "status-dot"}
          aria-hidden="true"
        />
        {DIGEST_OUTCOME_LABELS[entry.outcome]}
      </span>
      <span className="review-entry-main">
        <span className="review-entry-title">{entry.title}</span>
        {reason && <span className="muted review-entry-reason">{reason}</span>}
        {showError && <span className="muted review-entry-error">{entry.errorMessage}</span>}
      </span>
      {figures && (
        <span className="review-entry-figures tabular-nums">
          <span>{figures.duration}</span>
          <span>{figures.cost}</span>
        </span>
      )}
    </li>
  );
}
