import { useEffect, useState } from "react";

import { listRunsForTask, pruneRunLogs, toRimaiaError } from "../../lib/commands";
import { subscribeToRunsChanged } from "../../lib/events";
import { formatBytes } from "../../lib/format";
import type { PruneResult, RimaiaError, Run } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { RunDetailOverlay } from "../runs/RunDetailOverlay";
import { EXIT_CLASS_LABELS } from "./RunOutcomeSection";

interface RunHistorySectionProps {
  readonly taskId: string;
}

/**
 * Task 015's per-task history: every run of this task, newest first, each
 * opening {@link RunDetailOverlay} for the full outcome/diff/transcript
 * view. Additive to `RunOutcomeSection` (task 008's *last* run only) rather
 * than a replacement for it, the same file-ownership shape that section's
 * own doc comment describes.
 *
 * Also carries the "by task" half of ADR-0013's prune action — "by age"
 * lives in Settings' `StorageSection`, since an age cutoff is a global
 * housekeeping decision and a per-task purge is a decision about this one
 * task's history.
 */
export function RunHistorySection({ taskId }: RunHistorySectionProps) {
  const [runs, setRuns] = useState<Run[] | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [selectedRunId, setSelectedRunId] = useState<string | null>(null);
  const [pruning, setPruning] = useState(false);
  const [pruneResult, setPruneResult] = useState<PruneResult | null>(null);

  useEffect(() => {
    let active = true;

    function load() {
      listRunsForTask(taskId).then(
        (result) => {
          if (active) {
            setRuns(result);
            setError(null);
          }
        },
        (thrown) => {
          if (active) setError(toRimaiaError(thrown));
        },
      );
    }

    load();

    let unlisten: (() => void) | undefined;
    subscribeToRunsChanged(() => {
      if (active) load();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the initial
        // `load()` above is all this section will ever show.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, [taskId]);

  async function handlePrune() {
    setPruning(true);
    setPruneResult(null);
    try {
      const result = await pruneRunLogs({ kind: "task", taskId });
      setPruneResult(result);
      setError(null);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setPruning(false);
    }
  }

  return (
    <section className="task-detail-section run-history-section">
      <h4>Run history</h4>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {runs === null && !error && <p className="muted">Loading…</p>}
      {runs && runs.length === 0 && (
        <p className="muted">No runs yet — history appears here once one has started.</p>
      )}

      {runs && runs.length > 0 && (
        <>
          <ul className="run-history-list">
            {runs.map((run) => (
              <li key={run.id}>
                <button type="button" onClick={() => setSelectedRunId(run.id)}>
                  <span>Attempt {run.attempt}</span>
                  <span
                    className={
                      run.exitClass
                        ? `exit-class-badge exit-class-${run.exitClass}`
                        : "muted"
                    }
                  >
                    {run.exitClass ? EXIT_CLASS_LABELS[run.exitClass] : "Running"}
                  </span>
                  <span className="muted">{new Date(run.startedAt).toLocaleString()}</span>
                </button>
              </li>
            ))}
          </ul>

          <div className="run-history-actions">
            <button type="button" onClick={handlePrune} disabled={pruning}>
              {pruning ? "Pruning…" : "Prune this task's logs"}
            </button>
            {pruneResult && (
              <span className="muted">
                Removed {pruneResult.runsPruned} log
                {pruneResult.runsPruned === 1 ? "" : "s"}, freed{" "}
                {formatBytes(pruneResult.bytesFreed)}.
              </span>
            )}
          </div>
        </>
      )}

      {selectedRunId && (
        <RunDetailOverlay runId={selectedRunId} onClose={() => setSelectedRunId(null)} />
      )}
    </section>
  );
}
