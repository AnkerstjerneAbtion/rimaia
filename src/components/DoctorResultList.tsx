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
export function DoctorResultList({ results }: { results: readonly DoctorCheckResult[] }) {
  if (results.length === 0) return null;

  return (
    <ul className="doctor-results">
      {results.map((result) => (
        <li
          key={result.repository ? `${result.check}:${result.repository}` : result.check}
          className={`doctor-result doctor-result-${result.status}`}
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
        </li>
      ))}
    </ul>
  );
}
