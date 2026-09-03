import { DoctorResultList } from "../../components/DoctorResultList";
import { ErrorBanner } from "../../components/ErrorBanner";
import { useDoctor } from "../../hooks/useDoctor";
import { hasBlockingProblem } from "../../lib/doctor";

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
 */
export function DoctorSection() {
  const { report, running, error, checkedAt, rerun } = useDoctor();

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
          <DoctorResultList results={report.results} />
        </>
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
