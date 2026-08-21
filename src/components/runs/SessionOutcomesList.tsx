import type { ExitClass, RunStatus } from "../../types";
import { EXIT_CLASS_LABELS } from "../panel/RunOutcomeSection";

/**
 * One queue-supervised run this session has seen finish. Built by
 * `RunsView` from `getTask(taskId).lastRun`, never a row of its own — there
 * is no backend concept of "this session" (task 015's run history is the
 * durable version of this list), so this is exactly what `RunsView` observed
 * while it was mounted, not a fact stored anywhere.
 */
export interface SessionOutcome {
  readonly taskId: string;
  readonly title: string;
  readonly repositoryName: string;
  readonly status: RunStatus;
  readonly exitClass: ExitClass | null;
  readonly endedAt: string;
}

interface SessionOutcomesListProps {
  readonly outcomes: readonly SessionOutcome[];
}

/**
 * "What completed this session with its outcome" (task 009's Scope), newest
 * first. Reuses `RunOutcomeSection`'s own {@link EXIT_CLASS_LABELS} rather
 * than a second copy of ADR-0011's six exit-class phrases.
 */
export function SessionOutcomesList({ outcomes }: SessionOutcomesListProps) {
  if (outcomes.length === 0) {
    return (
      <p className="muted">
        Nothing has finished yet — this fills in as the queue works through the board.
      </p>
    );
  }

  return (
    <ul className="session-outcomes-list">
      {outcomes.map((outcome) => (
        <li key={`${outcome.taskId}-${outcome.endedAt}`} className="session-outcome-entry">
          <span className="session-outcome-title">{outcome.title}</span>
          <span className="session-outcome-repo">{outcome.repositoryName}</span>
          <span
            className={`exit-class-badge exit-class-${outcome.exitClass ?? "fatal"}`}
          >
            {outcome.exitClass ? EXIT_CLASS_LABELS[outcome.exitClass] : outcome.status}
          </span>
        </li>
      ))}
    </ul>
  );
}
