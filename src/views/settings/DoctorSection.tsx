import { DoctorResultList } from "../../components/DoctorResultList";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useDoctor } from "../../hooks/useDoctor";
import { dismissalFor, hasBlockingProblem, isStale } from "../../lib/doctor";

/**
 * Settings → Environment (task 018): the doctor's full report, passing rows
 * included, and a button to run it again.
 *
 * The banner shows only problems; this shows all eight checks. That is the
 * difference between the two surfaces and the reason both exist — a user who
 * has just fixed something needs to see the row turn green, and a user who
 * suspects a problem Rimaia has not reported needs to see that the check ran
 * at all and passed. A banner that rendered the passing rows too would be
 * eight lines of noise above the board every day.
 *
 * It is also **where a dismissed warning is found again** (task 027). A
 * dismissal the user cannot see is a leak rather than a feature, so the list
 * below carries the dismissed rows with a Restore control — and, separately,
 * any dismissal that no longer matches a row at all, because those are the ones
 * nothing else on either surface would ever mention.
 */
export function DoctorSection() {
  const { report, running, error, checkedAt, rerun, restore } = useDoctor();

  // A dismissal outlives the row it answered: the environment was fixed, or the
  // sentence changed and the warning came back as a new one. Either way the
  // stored entry is still there, still invisible, and still able to silence the
  // old sentence if it ever returns.
  const stale = report?.dismissals.filter((dismissal) => isStale(report, dismissal)) ?? [];

  return (
    <section className="panel">
      <h3>Environment</h3>
      <p className="muted">
        What an unattended run needs from this machine. Failures block the queue from starting;
        warnings do not.
      </p>

      {error && <ErrorBanner error={error} onDismiss={() => undefined} />}

      {report === null && running && <p className="muted">Checking…</p>}

      {report && (
        <>
          {hasBlockingProblem(report) && (
            <p className="doctor-blocked" role="alert">
              The queue cannot start until every failing check below is fixed.
            </p>
          )}
          <DoctorResultList
            results={report.results}
            onRestore={(result) => void restore(dismissalFor(result))}
          />
        </>
      )}

      {stale.length > 0 && (
        <div className="doctor-stale-dismissals">
          <h4>Dismissed warnings that no longer apply</h4>
          <p className="muted">
            These were dismissed and the checks have since stopped reporting them. Clearing one
            means the same sentence will be shown again if it ever comes back.
          </p>
          <ul className="doctor-stale-list">
            {stale.map((dismissal) => (
              <li
                key={`${dismissal.check}:${dismissal.repository ?? ""}:${dismissal.detail}`}
                className="doctor-stale-row"
              >
                <p>
                  {dismissal.detail}
                  {dismissal.repository && (
                    <span className="doctor-scope"> — {dismissal.repository}</span>
                  )}
                </p>
                <button
                  type="button"
                  className="doctor-result-action"
                  onClick={() => void restore(dismissal)}
                >
                  Clear
                </button>
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="doctor-actions">
        <button type="button" onClick={() => void rerun()} disabled={running}>
          {running ? "Checking…" : "Check again"}
        </button>
        {/* An "as of" line, because every input here lives outside the app and
            can go stale without anything to announce it — see `useDoctor`. */}
        {checkedAt && (
          <span className="muted">Checked at {checkedAt.toLocaleTimeString()}</span>
        )}
      </div>
    </section>
  );
}
