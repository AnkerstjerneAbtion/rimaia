import type { QueueEntry, SkipReason } from "../../types";

/**
 * The phrase a card or the Runs view shows for a `ready` task the queue
 * passes over — one wording, reused by both `TaskCard`'s queued-position
 * indicator and {@link QueuePlanList} below, so "why is this not queued"
 * never drifts into two independent copies.
 *
 * Mirrors `rimaia_core::scheduler::selection::SkipReason::explanation`'s own
 * audience split: that sentence answers a *button's* refusal (`TaskCard`'s
 * "Run now", seam-contract D8's "specificity lives in the message"), this one
 * answers "why is this card not in the queue" on a board where the
 * repository is already visible — same short register the backend's own doc
 * asks for, not a restatement of the refusal sentence.
 */
export const QUEUE_SKIP_LABELS: Record<SkipReason, string> = {
  unattended_runs_not_allowed: "this repository has not enabled unattended agent runs",
  dependency_not_satisfied: "waiting on a dependency",
  already_in_flight: "already started",
  waiting_for_retry: "waiting to resume",
  needs_attention: "the last run did not succeed",
};

interface QueuePlanListProps {
  readonly plan: readonly QueueEntry[];
}

/**
 * "The ordered list of what is next" (task 009's Scope) — every `ready` task
 * in board order, exactly as `getQueueStatus` returns it. A claimable task
 * shows the position it will run in; a skipped one shows why, in the same
 * list rather than a separate one, because the order is what tells the user
 * which skipped task is sitting ahead of which claimable one.
 */
export function QueuePlanList({ plan }: QueuePlanListProps) {
  if (plan.length === 0) {
    return <p className="muted">Nothing in the ready column right now.</p>;
  }

  return (
    <ol className="queue-plan-list">
      {plan.map((entry) => (
        <li key={entry.taskId} className={entryClassName(entry)}>
          {/* The ordinal sits in a gutter of its own so the titles line up
              whether or not a row has one. A skipped row gets a mark rather
              than a blank, because an empty gutter reads as a missing number
              instead of as "this one is not in the count". */}
          {entry.queuePosition !== null ? (
            <span className="queue-plan-position">#{entry.queuePosition}</span>
          ) : (
            <span className="queue-plan-skipmark" aria-hidden="true">
              ·
            </span>
          )}
          <span className="queue-plan-title">{entry.title}</span>
          {entry.skip !== null && (
            <span className="queue-plan-skip">Not queued — {QUEUE_SKIP_LABELS[entry.skip]}</span>
          )}
          {/* Only ever populated for a task in `waiting_retry` (D22), so this
              is "when it comes back", never a stale deadline on a task
              somebody has since started by hand. */}
          {entry.resumeAfter !== null && (
            <span className="queue-plan-resume">Resumes {resumeTime(entry.resumeAfter)}</span>
          )}
        </li>
      ))}
    </ol>
  );
}

/** The row's own classes. `-next` is the one task the queue will actually
 *  claim next: ADR-0007 makes board order execution order, so that is a fact
 *  the user set by dragging, and worth saying out loud. */
function entryClassName(entry: QueueEntry): string {
  if (entry.skip !== null) return "queue-plan-entry queue-plan-entry-skipped";
  if (entry.queuePosition === 1) return "queue-plan-entry queue-plan-entry-next";
  return "queue-plan-entry";
}

/** Local, to the minute. A resume time is about tonight; the date beside it
 *  would be noise. */
function resumeTime(iso: string): string {
  return new Date(iso).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}
