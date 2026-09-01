import { useState } from "react";

import type { ExitClass, Run } from "../../types";

/**
 * ADR-0011's six exit classes, in the words a reviewer reads rather than the
 * enum's own spelling. Exported so `components/runs` (task 008's live view)
 * renders a *finished* run's outcome with the same words instead of a second,
 * drifting copy.
 */
export const EXIT_CLASS_LABELS: Record<ExitClass, string> = {
  success: "Succeeded",
  usage_limit: "Stopped — usage limit reached",
  transient: "Stopped — transient error",
  interrupted: "Interrupted",
  fatal: "Failed",
  cancelled: "Cancelled",
};

/**
 * `$0.1503`, not `$0.15`. The spike's two environment measurements — $0.1061
 * inherited against $0.0291 isolated — differ starting at the third decimal
 * place, which two-decimal rounding would erase entirely, and per-run cost is
 * the one number a user checks against a bill. Exported so a mismatch between
 * this and a rendered value is a one-function diff to fix.
 */
export function formatCostUsd(costUsd: number): string {
  return `$${costUsd.toFixed(4)}`;
}

interface RunOutcomeSectionProps {
  readonly lastRun: Run | null;
  readonly loading: boolean;
}

/**
 * Task 008's own scope: "task detail shows the last run's outcome and links
 * to its log." Its own section rather than folded into task 007's
 * `RunInfoSection` (branch, worktree path, and a one-line status already read
 * off `lastRun.status`) — that section belongs to a different stage, and this
 * stage's file ownership is additive here, not a rewrite of it. Everything
 * shown below — the exit class, the cost, the error text, the PR link, the
 * log path — comes off the same `Run` row `TaskDetailPanel` already fetched
 * through `get_task`; this component makes no request of its own.
 */
export function RunOutcomeSection({ lastRun, loading }: RunOutcomeSectionProps) {
  return (
    <section className="task-detail-section run-outcome-section">
      <h4>Last run outcome</h4>
      {loading && <p className="muted">Loading…</p>}
      {!loading && !lastRun && (
        <p className="muted">No runs yet — an outcome appears here once one has finished.</p>
      )}
      {!loading && lastRun && <RunOutcomeDetails run={lastRun} />}
    </section>
  );
}

function RunOutcomeDetails({ run }: { readonly run: Run }) {
  const [copied, setCopied] = useState(false);

  async function handleCopyLogPath() {
    try {
      await navigator.clipboard.writeText(run.logPath);
      setCopied(true);
    } catch {
      // The path is still shown as text below — copying is a convenience on
      // top of that, never the only way to reach it (no backend command
      // exists to open it; task 015 owns the transcript viewer).
      setCopied(false);
    }
  }

  return (
    <>
      <dl className="detail-list">
        <dt>Outcome</dt>
        <dd>
          {run.exitClass ? (
            <span className={`exit-class-badge exit-class-${run.exitClass}`}>
              {EXIT_CLASS_LABELS[run.exitClass]}
            </span>
          ) : (
            <span className="muted">Still running.</span>
          )}
        </dd>
        <dt>Cost</dt>
        <dd>
          {run.costUsd != null ? (
            formatCostUsd(run.costUsd)
          ) : (
            <span className="muted">Not available yet.</span>
          )}
        </dd>
        <dt>Turns</dt>
        <dd>{run.numTurns ?? <span className="muted">—</span>}</dd>
        {run.errorMessage && (
          <>
            <dt>Error</dt>
            <dd className="run-outcome-error">{run.errorMessage}</dd>
          </>
        )}
        {run.prUrl && (
          <>
            <dt>Pull request</dt>
            <dd>
              <a href={run.prUrl} target="_blank" rel="noreferrer">
                {run.prUrl}
              </a>
            </dd>
          </>
        )}
        <dt>Log</dt>
        <dd>
          <code>{run.logPath}</code>
        </dd>
      </dl>
      <div className="run-outcome-actions">
        <button type="button" onClick={handleCopyLogPath}>
          {copied ? "Copied" : "Copy log path"}
        </button>
      </div>
    </>
  );
}
