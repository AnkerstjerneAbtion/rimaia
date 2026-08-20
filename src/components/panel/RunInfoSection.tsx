import { relativeTime } from "../../lib/board";
import type { Run } from "../../types";

const RUN_STATUS_LABELS: Record<Run["status"], string> = {
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
};

interface RunInfoSectionProps {
  readonly branch: string | null;
  readonly worktreePath: string | null;
  readonly lastRun: Run | null;
  readonly loading: boolean;
}

/**
 * Read-only, deliberately: branch, worktree path and the last run's outcome
 * are written by tasks 007 and 008, nothing in this panel. Every field is
 * `null` until those land, and task 005's own instruction is to render that
 * case on purpose — so each `dd` below says *why* it is empty, not just a
 * blank dash.
 */
export function RunInfoSection({ branch, worktreePath, lastRun, loading }: RunInfoSectionProps) {
  return (
    <section className="task-detail-section">
      <h4>Run info</h4>
      <dl className="detail-list">
        <dt>Branch</dt>
        <dd>
          {branch ? (
            <code>{branch}</code>
          ) : (
            <span className="muted">Not created yet — the first run creates it (task 007).</span>
          )}
        </dd>
        <dt>Worktree</dt>
        <dd>
          {worktreePath ? (
            <code>{worktreePath}</code>
          ) : (
            <span className="muted">Not created yet.</span>
          )}
        </dd>
        <dt>Last run</dt>
        <dd>
          {loading && <span className="muted">Loading…</span>}
          {!loading && lastRun && (
            <>
              {RUN_STATUS_LABELS[lastRun.status]}
              {lastRun.endedAt && (
                <span className="muted"> · {relativeTime(lastRun.endedAt, new Date())}</span>
              )}
            </>
          )}
          {!loading && !lastRun && <span className="muted">No runs yet (task 008).</span>}
        </dd>
      </dl>
    </section>
  );
}
