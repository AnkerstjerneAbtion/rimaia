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
  const [confirmingPrune, setConfirmingPrune] = useState(false);
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
    setConfirmingPrune(false);
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
                  {/* ADR-0011: the history of an overnight task "reads as the
                      sequence of walls it hit", and a wall is only legible with
                      the wait beside it — four attempts an hour apart and four
                      in the same minute are different stories. Absent on the
                      attempt that finally succeeded, which is what its absence
                      means. */}
                  {run.resumeAfter && (
                    <span className="muted run-history-wait">
                      waited until {new Date(run.resumeAfter).toLocaleTimeString()}
                    </span>
                  )}
                </button>
              </li>
            ))}
          </ul>

          {/* Deleting files gets the house confirm gate, the same one
              `worktree::ForceRemoval::ConfirmedByUser` exists for: the
              dangerous step is the one the user has to name. Two states of
              one control rather than a dialog — the panel is not a modal
              surface (`TaskDetailPanel`'s own doc), and a `window.confirm`
              would be the app's only native one. */}
          <div className="run-history-actions">
            {confirmingPrune ? (
              <>
                <span>
                  Delete every finished run's transcript for this task? The history above
                  stays; a run still in flight keeps its log.
                </span>
                <button type="button" onClick={handlePrune} disabled={pruning}>
                  {pruning ? "Pruning…" : "Delete transcripts"}
                </button>
                <button type="button" onClick={() => setConfirmingPrune(false)}>
                  Cancel
                </button>
              </>
            ) : (
              <button type="button" onClick={() => setConfirmingPrune(true)} disabled={pruning}>
                {pruning ? "Pruning…" : "Prune this task's logs"}
              </button>
            )}
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
