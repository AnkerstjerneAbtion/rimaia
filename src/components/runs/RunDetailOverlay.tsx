import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import {
  getRun,
  revealRunLog,
  summarizeRunTranscript,
  toRimaiaError,
} from "../../lib/commands";
import { EXIT_CLASS_LABELS, formatCostUsd } from "../panel/RunOutcomeSection";
import type { RimaiaError, RunDetail, TranscriptSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { TranscriptViewer } from "./TranscriptViewer";

interface RunDetailOverlayProps {
  readonly runId: string;
  readonly onClose: () => void;
}

/**
 * Task 015's run detail view, in ADR-0013's order: outcome, diff summary,
 * commits, PR link, the exact prompt, then the transcript — "diff and
 * commits before transcript is the whole design ... reviewing means looking
 * at the change; the transcript is for when the diff raises a question."
 *
 * An overlay rather than a route: this app has "three views with no URLs, no
 * nesting and no deep links to preserve" (`App.tsx`'s own comment), so a run
 * detail is a drawer painted over whichever view opened it — `TaskDetailPanel`
 * from a task's own history list, `RunsView` from the global one — the same
 * non-modal-drawer shape `TaskDetailPanel` itself uses over the board.
 *
 * **Portalled to `document.body`, not rendered where it is written.** Opened
 * from the Runs view it is already a child of the page; opened from the task
 * panel it would be a child of `.task-detail-panel`, which is a scroll
 * container (`overflow-y: auto`) and its own stacking context (`z-index: 2`).
 * A viewport overlay nested inside one of those is at the mercy of how the
 * engine treats a fixed descendant of a scrolled, clipping ancestor — in
 * WKWebView, which is the only engine this app ever runs in, it is laid out
 * against that ancestor rather than the viewport, so opening a run from the
 * board painted the overlay at the top of a panel the reader had scrolled
 * past: the panel appeared to blank and the detail was nowhere. The portal is
 * what makes "over whichever view opened it" true of both mount points
 * instead of only the one this component was first written under.
 */
export function RunDetailOverlay({ runId, onClose }: RunDetailOverlayProps) {
  const [detail, setDetail] = useState<RunDetail | null>(null);
  const [summary, setSummary] = useState<TranscriptSummary | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [revealing, setRevealing] = useState(false);
  const [revealError, setRevealError] = useState<RimaiaError | null>(null);
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    let active = true;
    getRun(runId).then(
      (result) => {
        if (active) {
          setDetail(result);
          setError(null);
        }
      },
      (thrown) => {
        if (active) setError(toRimaiaError(thrown));
      },
    );
    return () => {
      active = false;
    };
  }, [runId]);

  // A second read rather than a field on `getRun`, because it costs a scan of
  // the transcript. Its own failure is deliberately silent: the summary
  // explains the run, it is not the run, and a pruned log must not put an
  // error banner over an outcome that is perfectly readable without it.
  useEffect(() => {
    let active = true;
    setSummary(null);
    summarizeRunTranscript(runId).then(
      (result) => {
        if (active) setSummary(result);
      },
      () => {},
    );
    return () => {
      active = false;
    };
  }, [runId]);

  async function handleReveal() {
    setRevealing(true);
    setRevealError(null);
    try {
      await revealRunLog(runId);
    } catch (thrown) {
      setRevealError(toRimaiaError(thrown));
    } finally {
      setRevealing(false);
    }
  }

  async function handleCopyPath() {
    if (!detail) return;
    try {
      await navigator.clipboard.writeText(detail.logPath);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  }

  return createPortal(
    <div className="run-detail-overlay" role="dialog" aria-label="Run detail">
      <div className="run-detail-overlay-header">
        <h3>Run detail{detail ? ` — attempt ${detail.attempt}` : ""}</h3>
        <button type="button" className="run-detail-close" onClick={onClose} aria-label="Close">
          Esc
        </button>
      </div>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {!detail && !error && <p className="muted">Loading run detail…</p>}

      {detail && (
        <div className="run-detail-body">
          <section className="run-detail-section">
            <h4>Outcome</h4>
            {/* The four facts that identify a finished run, as labelled
                readouts rather than as a two-column `<dl>` that spent a third
                of the rail on the words "Duration" and "Turns" — the same
                shape the board drawer's own `.task-detail-readout` uses. The
                `<dl>` underneath is kept for the fields that are sentences
                rather than figures, and that only sometimes exist. */}
            <div className="run-detail-readout">
              <div className="run-detail-readout-field">
                <span className="run-detail-readout-label">Status</span>
                <span className="run-detail-readout-value">
                  {detail.exitClass ? (
                    <span className={`exit-class-badge exit-class-${detail.exitClass}`}>
                      {EXIT_CLASS_LABELS[detail.exitClass]}
                    </span>
                  ) : (
                    <span className="muted">Still running.</span>
                  )}
                </span>
              </div>
              <div className="run-detail-readout-field">
                <span className="run-detail-readout-label">Duration</span>
                <span className="run-detail-readout-value">
                  {formatDuration(detail.startedAt, detail.endedAt)}
                </span>
              </div>
              <div className="run-detail-readout-field">
                <span className="run-detail-readout-label">Turns</span>
                <span className="run-detail-readout-value">
                  {detail.numTurns ?? <span className="muted">—</span>}
                </span>
              </div>
              <div className="run-detail-readout-field">
                <span className="run-detail-readout-label">Cost</span>
                <span className="run-detail-readout-value">
                  {detail.costUsd != null ? (
                    formatCostUsd(detail.costUsd)
                  ) : (
                    <span className="muted">Not available yet.</span>
                  )}
                </span>
              </div>
            </div>
            <dl className="detail-list">
              {detail.errorMessage && (
                <>
                  <dt>Error</dt>
                  <dd className="run-outcome-error">{detail.errorMessage}</dd>
                </>
              )}
              {/* What the transcript knows that the row does not. A run that
                  was refused every command it tried still produces an
                  unremarkable-looking row; these three lines are where that
                  becomes visible without reading a thousand entries. */}
              {summary?.permissionMode && (
                <>
                  <dt>Ran as</dt>
                  <dd>
                    {summary.permissionMode}
                    {summary.model && <span className="muted"> · {summary.model}</span>}
                  </dd>
                </>
              )}
              {summary && summary.deniedToolCalls > 0 && (
                <>
                  <dt>Refused</dt>
                  <dd className="run-outcome-error">
                    {summary.deniedToolCalls} tool call
                    {summary.deniedToolCalls === 1 ? " was" : "s were"} refused for want of
                    approval — this run could do nothing its permission mode had not already
                    allowed.
                  </dd>
                </>
              )}
              {summary && !summary.endedWithResult && (
                <>
                  <dt>Stream</dt>
                  <dd>
                    {summary.endsMidLine
                      ? "The transcript ends mid-line: the CLI stopped writing before it reported a result."
                      : "The stream ended without a result event, so this outcome was inferred rather than reported."}
                  </dd>
                </>
              )}
            </dl>
          </section>

          <section className="run-detail-section">
            <h4>Diff summary</h4>
            <p>
              {detail.diff.diff.filesChanged} {detail.diff.diff.filesChanged === 1 ? "file" : "files"}{" "}
              changed (+{detail.diff.diff.insertions} / -{detail.diff.diff.deletions})
            </p>
            {detail.diff.files.length > 0 && (
              <ul className="run-detail-file-list">
                {detail.diff.files.map((file) => (
                  <li key={file.path}>
                    <code>{file.path}</code>
                    {file.insertions == null ? (
                      <span className="muted">binary</span>
                    ) : (
                      <span className="run-detail-diffstat">
                        +{file.insertions} / -{file.deletions}
                      </span>
                    )}
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="run-detail-section">
            <h4>Commits</h4>
            {detail.diff.commits.length === 0 ? (
              <p className="muted">No commits on this branch yet.</p>
            ) : (
              <ul className="run-detail-commit-list">
                {detail.diff.commits.map((commit) => (
                  <li key={commit.sha}>
                    {/* One flex item per column, not a text node between two
                        elements: an anonymous flex item cannot be aligned or
                        truncated, and the author has to sit against the right
                        edge however long the subject is. */}
                    <span>
                      <code>{commit.shortSha}</code> {commit.subject}
                    </span>
                    <span className="run-detail-diffstat">{commit.author}</span>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="run-detail-section">
            <h4>Pull request</h4>
            {detail.prUrl ? (
              <a href={detail.prUrl} target="_blank" rel="noreferrer">
                {detail.prUrl}
              </a>
            ) : (
              <p className="muted">No pull request opened yet.</p>
            )}
          </section>

          <section className="run-detail-section">
            <h4>Prompt</h4>
            <pre className="run-detail-prompt">{detail.prompt}</pre>
          </section>

          <section className="run-detail-section">
            <h4>Transcript</h4>
            <div className="run-detail-actions">
              <button
                type="button"
                onClick={handleReveal}
                disabled={!detail.logAvailable || revealing}
              >
                {/* "Reveal", not "open": the backend shows the file in the
                    OS file manager rather than handing megabytes of JSONL to
                    whatever is registered for the extension — see
                    `commands::runs::reveal_run_log`. */}
                {revealing ? "Revealing…" : "Reveal raw log"}
              </button>
              <button type="button" onClick={handleCopyPath}>
                {copied ? "Copied" : "Copy log path"}
              </button>
            </div>
            {revealError && (
              <ErrorBanner error={revealError} onDismiss={() => setRevealError(null)} />
            )}
            {detail.logAvailable ? (
              <TranscriptViewer runId={runId} />
            ) : (
              <p className="muted">
                Log unavailable — the transcript file was deleted or pruned.
              </p>
            )}
          </section>
        </div>
      )}
    </div>,
    document.body,
  );
}

/** `"12m 34s"`, `"3s"`, or `"in progress"` while the run has not ended. */
function formatDuration(startedAt: string, endedAt: string | null): string {
  if (!endedAt) return "In progress.";
  const totalSeconds = Math.max(
    0,
    Math.round((new Date(endedAt).getTime() - new Date(startedAt).getTime()) / 1000),
  );
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return minutes > 0 ? `${minutes}m ${seconds}s` : `${seconds}s`;
}
