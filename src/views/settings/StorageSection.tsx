import { useEffect, useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getAppInfo,
  getRunLogSize,
  pruneRunLogs,
  revealAppDataDir,
  toRimaiaError,
} from "../../lib/commands";
import { formatBytes } from "../../lib/format";
import type { AppInfo, PruneResult, RimaiaError } from "../../types";

/** Presets task 015's "by age" prune offers — a plain number input inviting
 *  an arbitrary value is more to get wrong (0, a negative number, a typo
 *  with an extra digit) than a short, reviewable list of sensible ages. */
const PRUNE_AGE_PRESETS = [
  { label: "Older than 7 days", days: 7 },
  { label: "Older than 30 days", days: 30 },
  { label: "Older than 90 days", days: 90 },
];

export function StorageSection() {
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [logSize, setLogSize] = useState<number | null>(null);
  const [pruning, setPruning] = useState(false);
  /** The preset awaiting confirmation, or `null` when none is. */
  const [confirmingDays, setConfirmingDays] = useState<number | null>(null);
  const [pruneResult, setPruneResult] = useState<PruneResult | null>(null);

  useEffect(() => {
    getAppInfo().then(setInfo, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  function loadLogSize() {
    getRunLogSize().then(setLogSize, (thrown) => setError(toRimaiaError(thrown)));
  }

  useEffect(loadLogSize, []);

  async function openInFinder() {
    setError(null);
    try {
      await revealAppDataDir();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  async function handlePrune(days: number) {
    setPruning(true);
    setConfirmingDays(null);
    setPruneResult(null);
    try {
      const result = await pruneRunLogs({ kind: "older_than_days", days });
      setPruneResult(result);
      setError(null);
      // The whole point of reporting a size here at all: a prune that ran
      // must be reflected in the number right below it, not just in the
      // one-off "removed N, freed M" line.
      loadLogSize();
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setPruning(false);
    }
  }

  return (
    <section className="panel">
      <h3>Storage</h3>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {info ? (
        <>
          <dl className="detail-list">
            <dt>Application data</dt>
            <dd>
              <code>{info.dataDir}</code>
            </dd>
            <dt>Database</dt>
            <dd>
              <code>{info.dbFile}</code>
            </dd>
            <dt>Logs</dt>
            <dd>
              <code>{info.logsDir}</code>
            </dd>
            <dt>Version</dt>
            <dd>{info.appVersion}</dd>
          </dl>
          <button type="button" onClick={openInFinder}>
            Open in Finder
          </button>
        </>
      ) : (
        !error && <p className="muted">Reading…</p>
      )}

      {/* Task 015's housekeeping: total run-log size alongside worktree
          size, with a prune-by-age action. Pruning by task lives on the
          task detail panel's own history section instead — a per-task
          purge is that task's own decision, not a global one. */}
      <dl className="detail-list">
        <dt>Run logs</dt>
        <dd>{logSize === null ? <span className="muted">Reading…</span> : formatBytes(logSize)}</dd>
      </dl>
      {/* Confirmed before it runs, like every other action in this app that
          deletes files (`worktree::ForceRemoval::ConfirmedByUser`) — and this
          one spans every task, not one. */}
      <div className="storage-prune-actions">
        {confirmingDays === null ? (
          PRUNE_AGE_PRESETS.map((preset) => (
            <button
              key={preset.days}
              type="button"
              onClick={() => setConfirmingDays(preset.days)}
              disabled={pruning}
            >
              {preset.label}
            </button>
          ))
        ) : (
          <>
            <span>
              Delete the transcript of every finished run started more than {confirmingDays} days
              ago, across every task? The run history itself stays.
            </span>
            <button type="button" onClick={() => handlePrune(confirmingDays)} disabled={pruning}>
              {pruning ? "Pruning…" : "Delete transcripts"}
            </button>
            <button type="button" onClick={() => setConfirmingDays(null)}>
              Cancel
            </button>
          </>
        )}
      </div>
      {pruneResult && (
        <p className="muted">
          Removed {pruneResult.runsPruned} log{pruneResult.runsPruned === 1 ? "" : "s"}, freed{" "}
          {formatBytes(pruneResult.bytesFreed)}.
        </p>
      )}
    </section>
  );
}
