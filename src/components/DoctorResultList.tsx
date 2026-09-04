import { statusLabel } from "../lib/doctor";
import type { DoctorCheckResult } from "../types";

/**
 * The rows of a doctor report, rendered the same way everywhere they appear —
 * the banner, the Settings section and each welcome step share this list so a
 * failure reads identically wherever the user meets it.
 *
 * `remediation` is rendered as the row's own paragraph rather than folded into
 * `detail`, because the two do different jobs: `detail` says what is wrong,
 * `remediation` says what to type. Task 018's acceptance criteria turn on the
 * second one being present and specific, so it gets its own element rather
 * than being concatenated into a sentence.
 */
export function DoctorResultList({
  results,
  onDismiss,
  onRestore,
}: {
  results: readonly DoctorCheckResult[];
  /**
   * Offered on `warn` rows only (task 027). A `fail` collapses instead — see
   * {@link DoctorBanner} — and a `pass` is not in the banner to begin with.
   */
  onDismiss?: (result: DoctorCheckResult) => void;
  /** Offered on rows already dismissed, which only Settings → Environment shows. */
  onRestore?: (result: DoctorCheckResult) => void;
}) {
  if (results.length === 0) return null;

  return (
    <ul className="doctor-results">
      {results.map((result) => (
        <li
          key={result.repository ? `${result.check}:${result.repository}` : result.check}
          className={`doctor-result doctor-result-${result.status}${
            result.dismissed ? " doctor-result-dismissed" : ""
          }`}
        >
          <span className={`doctor-status doctor-status-${result.status}`}>
            {statusLabel(result.status)}
          </span>
          <div className="doctor-result-body">
            <h4>
              {result.label}
              {/* The repository is part of the heading, not only of `detail`:
                  eight rows that all say "GitHub CLI" are unreadable on a
                  machine with four repositories registered. */}
              {result.repository && <span className="doctor-scope"> — {result.repository}</span>}
            </h4>
            <p>{result.detail}</p>
            {result.remediation && <p className="doctor-remediation">{result.remediation}</p>}
          </div>
          {onDismiss && result.status === "warn" && !result.dismissed && (
            <button
              type="button"
              className="doctor-result-action"
              onClick={() => onDismiss(result)}
            >
              Dismiss
            </button>
          )}
          {onRestore && result.dismissed && (
            <button
              type="button"
              className="doctor-result-action"
              onClick={() => onRestore(result)}
            >
              Restore
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}
