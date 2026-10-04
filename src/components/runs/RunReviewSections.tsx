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
  /** Whether a `not_recorded` review may ask the local `get_diff_summary` for
   *  the branch's current state. The overlay says `fallback`; the morning
   *  review says `none`, because it must render from the bundle on every
   *  client, and the browser has no such command (ADR-0033 point 7). Changes
   *  nothing for a recorded review, which never asks under either value. */
  readonly liveDiff: "fallback" | "none";
  /** `collapsed` keeps the patch in a closed `<details>` inside the diff
   *  section, for an overlay with a transcript to get to. `expanded` puts it
   *  in a section of its own after the pull request, in ADR-0013's order. */
  readonly patch: "collapsed" | "expanded";
}

/**
 * The diff, commits and pull request sections of a run's review, rendered from
 * what the run's finish recorded (task 033, ADR-0033 point 7) rather than from
 * the branch as it is now. One renderer for the run detail overlay and the
 * morning review (task 017), so the two cannot show the same bundle two ways.
 *
 * Separate `<section>`s rather than one, so ADR-0013's order — outcome, diff,
 * commits, PR, prompt, transcript — stays the callers' own headings.
 *
 * **The one live fallback.** A `not_recorded` row (a run from before bundles,
 * one in flight, one a crash closed, one whose capture failed) may ask the
 * local `get_diff_summary` command for the branch's current state, and says that
 * is what it is showing — but only when `liveDiff` is `fallback`. A recorded row
 * never asks: what it shows must not change because the branch did.
 */
export function RunReviewSections({
  taskId,
  review,
  prUrl,
  liveDiff,
  patch,
}: RunReviewSectionsProps) {
  if (review.source === "not_recorded") {
    return liveDiff === "fallback" ? (
      <LiveFallback taskId={taskId} prUrl={prUrl} />
    ) : (
      <>
        <section className="run-detail-section">
          <h4>Diff summary</h4>
          <p className="muted">No diff was recorded for this run.</p>
        </section>
        <section className="run-detail-section">
          <h4>Commits</h4>
          <p className="muted">None recorded.</p>
        </section>
        <PullRequestSection prUrl={prUrl} />
      </>
    );
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
        <PullRequestSection prUrl={prUrl} />
      </>
    );
  }
  return <RecordedBundle bundle={review.bundle} prUrl={prUrl} patch={patch} />;
}

function PullRequestSection({ prUrl }: { prUrl: string | null }) {
  return (
    <section className="run-detail-section">
      <h4>Pull request</h4>
      {prUrl ? (
        <a href={prUrl} target="_blank" rel="noreferrer">
          {prUrl}
        </a>
      ) : (
        <p className="muted">No pull request opened yet.</p>
      )}
    </section>
  );
}

function RecordedBundle({
  bundle,
  prUrl,
  patch,
}: {
  bundle: StoredBundle;
  prUrl: string | null;
  patch: "collapsed" | "expanded";
}) {
  const included = bundle.files.filter((file) => file.patch === "included").length;
  const pruned = bundle.patchPrunedAt !== null;
  const truncatedNote = !pruned && bundle.patchTruncated && (
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
        "The rest was not stored. It is on the branch."
      )}
    </p>
  );
  const prunedNote = pruned && (
    <p className="muted run-detail-review-note">
      The patch was pruned on {formatDate(bundle.patchPrunedAt as string)}. The file list and
      commits are kept.
    </p>
  );
  return (
    <>
      <section className="run-detail-section">
        <h4>Diff summary</h4>
        <p>{totals(bundle.diff)}</p>
        {patch === "collapsed" && prunedNote}
        {patch === "collapsed" && truncatedNote}
        <FileList files={bundle.files} />
        {patch === "collapsed" && bundle.patch && (
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
      <PullRequestSection prUrl={prUrl} />
      {/* Task 037 slots the review findings here, after the pull request and
          before the patch: findings are read with the PR link, not below a
          screenful of diff. Nothing renders in this place until then. */}
      {patch === "expanded" && (bundle.patch || prunedNote || truncatedNote) && (
        <section className="run-detail-section">
          <h4>Patch</h4>
          {bundle.patch && (
            <pre className="run-detail-patch-text run-detail-patch-expanded">{bundle.patch}</pre>
          )}
          {prunedNote}
          {truncatedNote}
        </section>
      )}
    </>
  );
}

type Fallback =
  | { state: "loading" }
  | { state: "read"; summary: DiffSummary }
  | { state: "unreadable" };

function LiveFallback({ taskId, prUrl }: { taskId: string; prUrl: string | null }) {
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
        <PullRequestSection prUrl={prUrl} />
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
      <PullRequestSection prUrl={prUrl} />
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
