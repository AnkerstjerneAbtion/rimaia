import { useEffect, useState } from "react";

import { getRun, revealRunLog, toRimaiaError } from "../../lib/commands";
import { EXIT_CLASS_LABELS, formatCostUsd } from "../panel/RunOutcomeSection";
import type { RimaiaError, RunDetail } from "../../types";
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
 */
export function RunDetailOverlay({ runId, onClose }: RunDetailOverlayProps) {
  const [detail, setDetail] = useState<RunDetail | null>(null);
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

  return (
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
        <>
          <section className="run-detail-section">
            <h4>Outcome</h4>
            <dl className="detail-list">
              <dt>Status</dt>
              <dd>
                {detail.exitClass ? (
                  <span className={`exit-class-badge exit-class-${detail.exitClass}`}>
                    {EXIT_CLASS_LABELS[detail.exitClass]}
                  </span>
                ) : (
                  <span className="muted">Still running.</span>
                )}
              </dd>
              <dt>Duration</dt>
              <dd>{formatDuration(detail.startedAt, detail.endedAt)}</dd>
              <dt>Turns</dt>
              <dd>{detail.numTurns ?? <span className="muted">—</span>}</dd>
              <dt>Cost</dt>
              <dd>
                {detail.costUsd != null ? (
                  formatCostUsd(detail.costUsd)
                ) : (
                  <span className="muted">Not available yet.</span>
                )}
              </dd>
              {detail.errorMessage && (
                <>
                  <dt>Error</dt>
                  <dd className="run-outcome-error">{detail.errorMessage}</dd>
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
                    <code>{file.path}</code>{" "}
                    {file.insertions == null ? (
                      <span className="muted">binary</span>
                    ) : (
                      <span>
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
                    <code>{commit.shortSha}</code> {commit.subject}{" "}
                    <span className="muted">— {commit.author}</span>
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
                {revealing ? "Opening…" : "Open raw log"}
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
        </>
      )}
    </div>
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
