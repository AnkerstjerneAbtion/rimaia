import type { ReactNode } from "react";

import type { ReviewTarget } from "../../hooks/useReviewTarget";
import { relativeTime } from "../../lib/board";
import { formatSeconds, runOutcome } from "../../lib/review";
import type { BoardColumn, TaskSummary } from "../../types";
import { COLUMN_TITLES } from "../board/Column";
import { formatCostUsd } from "../panel/RunOutcomeSection";
import { RunReviewSections } from "../runs/RunReviewSections";

interface TaskReviewProps {
  readonly task: TaskSummary;
  readonly repositoryName: string | null;
  /** 1-based place in the queue, and its length. */
  readonly place: { readonly index: number; readonly total: number };
  readonly target: ReviewTarget | null;
  /** The board's read, for the titles and columns of the task's own
   *  dependencies — `get_task` lists only their ids. */
  readonly board: readonly TaskSummary[];
  readonly now: Date;
  /** Rendered between the title and the review: the actions and the note step.
   *  The review itself keeps ADR-0013's order whatever sits above it. */
  readonly children?: ReactNode;
}

/**
 * The task on screen, in ADR-0013's order without any navigation: outcome, the
 * diff summary with its file list, the commits, the pull request, then the
 * patch as plain text. All of it comes from the newest run's recorded review,
 * so it renders the same on a machine that does not hold the worktree.
 */
export function TaskReview({
  task,
  repositoryName,
  place,
  target,
  board,
  now,
  children,
}: TaskReviewProps) {
  return (
    <article className="review-task" aria-label={task.title}>
      <header className="review-task-head">
        <p className="muted review-task-place tabular-nums">
          {place.index} of {place.total}
          {repositoryName && <> · {repositoryName}</>}
        </p>
        <h3>{task.title}</h3>
        {target && <ChainLists target={target} board={board} />}
      </header>

      {children}

      {target === null ? (
        <p className="muted">Reading the run…</p>
      ) : (
        <div className="review-task-body">
          {target.run === null ? (
            <p className="review-no-run">This task has no run to review.</p>
          ) : (
            <>
              <OutcomeSection run={target.run} now={now} />
              <RunReviewSections
                taskId={target.task.id}
                review={target.run.review}
                prUrl={target.run.prUrl}
                liveDiff="none"
                patch="expanded"
              />
            </>
          )}
        </div>
      )}
    </article>
  );
}

function OutcomeSection({ run, now }: { run: NonNullable<ReviewTarget["run"]>; now: Date }) {
  const outcome = runOutcome(run);
  const seconds =
    run.endedAt === null
      ? null
      : (Date.parse(run.endedAt) - Date.parse(run.startedAt)) / 1000;
  return (
    <section className="run-detail-section">
      <h4>Outcome</h4>
      <p className="review-run-outcome">
        <span className={`review-run-state review-run-state-${outcome.tone}`}>
          <span className="status-dot" aria-hidden="true" />
          {outcome.label}
        </span>
        <span className="muted tabular-nums">
          Attempt {run.attempt}
          {run.endedAt && ` · ended ${relativeTime(run.endedAt, now)}`}
          {seconds !== null && Number.isFinite(seconds) && ` · ${formatSeconds(seconds)}`}
          {" · "}
          {run.costUsd === null ? "cost not recorded" : formatCostUsd(run.costUsd)}
        </span>
      </p>
      {run.errorMessage && <p className="run-outcome-error">{run.errorMessage}</p>}
    </section>
  );
}

function columnWord(column: BoardColumn): string {
  return COLUMN_TITLES[column];
}

/**
 * ADR-0008's chain, on the task being reviewed. Two lists with two different
 * words on purpose: "Builds on" is what this task depends on, "Already ran on
 * this branch" is a dependent that was already started from this task's work.
 */
function ChainLists({
  target,
  board,
}: {
  target: ReviewTarget;
  board: readonly TaskSummary[];
}) {
  const dependencies = target.task.dependsOn;
  const { dependents } = target;
  if (dependencies.length === 0 && dependents.length === 0) return null;
  return (
    <div className="review-chain">
      {dependencies.length > 0 && (
        <section aria-label="Builds on">
          <h4>Builds on</h4>
          <ul>
            {dependencies.map((id) => {
              const found = board.find((candidate) => candidate.id === id);
              return (
                <li key={id}>
                  {found ? found.title : "A task that is no longer on the board"}
                  {found && <span className="muted"> · {columnWord(found.column)}</span>}
                </li>
              );
            })}
          </ul>
        </section>
      )}
      {dependents.length > 0 && (
        <section aria-label="Depends on this">
          <h4>Depends on this</h4>
          <ul>
            {dependents.map((dependent) => (
              <li key={dependent.id}>
                {dependent.title}
                <span className="muted"> · {columnWord(dependent.column)}</span>
                {dependent.archivedAt !== null && (
                  <span className="review-chain-tag">Archived</span>
                )}
                {dependent.builtOn && (
                  <span className="review-chain-tag">Already ran on this branch</span>
                )}
              </li>
            ))}
          </ul>
        </section>
      )}
    </div>
  );
}
