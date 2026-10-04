import { useEffect, useState } from "react";

import { getDiffSummary } from "../../lib/commands";
import { formatBytes } from "../../lib/format";
import type {
  BundleFile,
  CommitSummary,
  DiffSummary,
  PatchInclusion,
  RunReview,
  StoredBundle,
} from "../../types";

interface RunReviewSectionsProps {
  readonly taskId: string;
  readonly review: RunReview;
  readonly prUrl: string | null;
}

/**
 * The diff and commits sections of the run detail overlay, rendered from what
 * the run's finish recorded (task 033, ADR-0033 point 7) rather than from the
 * branch as it is now.
 *
 * Two `<section>`s rather than one, so ADR-0013's order — outcome, diff,
 * commits, PR, prompt, transcript — stays the overlay's own headings.
 *
 * **The one live fallback.** A `not_recorded` row (a run from before bundles,
 * one in flight, one a crash closed, one whose capture failed) asks the local
 * `get_diff_summary` command for the branch's current state, and says that is
 * what it is showing. It lives here, in the desktop view that has a machine
 * behind it, and never in `get_run`, which every client reads. A recorded row
 * never asks: what it shows must not change because the branch did.
 */
export function RunReviewSections({ taskId, review, prUrl }: RunReviewSectionsProps) {
  if (review.source === "not_recorded") {
    return <LiveFallback taskId={taskId} />;
  }
  if (review.bundle === null) {
    return (
      <>
        <section className="run-detail-section">
          <h4>Diff summary</h4>
          <p className="muted">This run ended with no commits on its branch.</p>
        </section>
        <section className="run-detail-section">
          <h4>Commits</h4>
          <p className="muted">None.</p>
        </section>
      </>
    );
  }
  return <RecordedBundle bundle={review.bundle} prUrl={prUrl} />;
}

function RecordedBundle({ bundle, prUrl }: { bundle: StoredBundle; prUrl: string | null }) {
  const included = bundle.files.filter((file) => file.patch === "included").length;
  return (
    <>
      <section className="run-detail-section">
        <h4>Diff summary</h4>
        <p>{totals(bundle.diff)}</p>
        {bundle.patchPrunedAt !== null ? (
          <p className="muted run-detail-review-note">
            The patch was pruned on {formatDate(bundle.patchPrunedAt)}. The file list and
            commits are kept.
          </p>
        ) : (
          bundle.patchTruncated && (
            <p className="muted run-detail-review-note">
              The patch holds {included} of {bundle.files.length}{" "}
              {bundle.files.length === 1 ? "file" : "files"}; the whole diff was{" "}
              {formatBytes(bundle.patchBytes)}.{" "}
              {prUrl ? (
                <>
                  The rest is on the{" "}
                  <a href={prUrl} target="_blank" rel="noreferrer">
                    pull request
                  </a>
                  .
                </>
              ) : (
                "The rest was not stored."
              )}
            </p>
          )
        )}
        <FileList files={bundle.files} />
        {bundle.patch && (
          <details className="run-detail-patch">
            <summary>Patch</summary>
            <pre className="run-detail-patch-text">{bundle.patch}</pre>
          </details>
        )}
      </section>
      <section className="run-detail-section">
        <h4>Commits</h4>
        <CommitList commits={bundle.commits} empty="No commits on this branch." />
      </section>
    </>
  );
}

type Fallback =
  | { state: "loading" }
  | { state: "read"; summary: DiffSummary }
  | { state: "unreadable" };

function LiveFallback({ taskId }: { taskId: string }) {
  const [fallback, setFallback] = useState<Fallback>({ state: "loading" });

  useEffect(() => {
    let active = true;
    setFallback({ state: "loading" });
    getDiffSummary(taskId).then(
      (summary) => {
        if (active) setFallback({ state: "read", summary });
      },
      // Never an overlay error: the branch or the clone being gone is the
      // ordinary reason there is nothing to show, and the outcome, prompt and
      // transcript are still worth reading.
      () => {
        if (active) setFallback({ state: "unreadable" });
      },
    );
    return () => {
      active = false;
    };
  }, [taskId]);

  // Both sections even here: the overlay's grid places its sections by
  // position, and a missing one would move the prompt into the rail.
  if (fallback.state === "unreadable") {
    return (
      <>
        <section className="run-detail-section">
          <h4>Diff summary</h4>
          <p className="muted">
            No diff was recorded for this run, and its branch can no longer be read.
          </p>
        </section>
        <section className="run-detail-section">
          <h4>Commits</h4>
          <p className="muted">None recorded.</p>
        </section>
      </>
    );
  }

  return (
    <>
      <section className="run-detail-section">
        <h4>Diff summary</h4>
        <p className="muted run-detail-review-note">
          No diff was recorded for this run. This is the branch’s current state, not what this
          run left.
        </p>
        {fallback.state === "loading" ? (
          <p className="muted">Reading the branch…</p>
        ) : (
          <>
            <p>{totals(fallback.summary.diff)}</p>
            <FileList files={fallback.summary.files} />
          </>
        )}
      </section>
      <section className="run-detail-section">
        <h4>Commits</h4>
        {fallback.state === "read" && (
          <CommitList commits={fallback.summary.commits} empty="No commits on this branch yet." />
        )}
      </section>
    </>
  );
}

const LEFT_OUT: Record<Exclude<PatchInclusion, "included">, string> = {
  too_large: "not in patch: too large",
  binary: "not in patch: binary",
  not_utf8: "not in patch: not UTF-8 text",
};

/** A bundle's files carry their patch inclusion; a live summary's do not. */
function FileList({
  files,
}: {
  files: ReadonlyArray<BundleFile | Omit<BundleFile, "patch">>;
}) {
  if (files.length === 0) return null;
  return (
    <ul className="run-detail-file-list">
      {files.map((file) => {
        const inclusion = "patch" in file ? file.patch : "included";
        return (
          <li key={file.path}>
            <code>{file.path}</code>
            <span className="run-detail-diffstat">
              {inclusion !== "included" && (
                <span className="run-detail-patch-marker">{LEFT_OUT[inclusion]}</span>
              )}
              {file.insertions == null ? (
                inclusion === "included" && "binary"
              ) : (
                <span>
                  +{file.insertions} / -{file.deletions}
                </span>
              )}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function CommitList({ commits, empty }: { commits: CommitSummary[]; empty: string }) {
  if (commits.length === 0) return <p className="muted">{empty}</p>;
  return (
    <ul className="run-detail-commit-list">
      {commits.map((commit) => (
        <li key={commit.sha}>
          {/* One flex item per column, not a text node between two elements:
              an anonymous flex item cannot be aligned or truncated, and the
              author has to sit against the right edge however long the
              subject is. */}
          <span>
            <code>{commit.shortSha}</code> {commit.subject}
          </span>
          <span className="run-detail-diffstat">{commit.author}</span>
        </li>
      ))}
    </ul>
  );
}

function totals(diff: { filesChanged: number; insertions: number; deletions: number }): string {
  return `${diff.filesChanged} ${diff.filesChanged === 1 ? "file" : "files"} changed (+${
    diff.insertions
  } / -${diff.deletions})`;
}

function formatDate(instant: string): string {
  return new Date(instant).toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}
