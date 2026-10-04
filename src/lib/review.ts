import { QUEUE_SKIP_LABELS } from "../components/runs/QueuePlanList";
import { COLUMN_TITLES } from "../components/board/Column";
import { groupIntoColumns, relativeTime } from "./board";
import type { BoardCard } from "./board";
import type {
  DigestEntry,
  DigestOutcome,
  ExitClass,
  ReviewDigest,
  RunStatus,
  TaskDependent,
  View,
} from "../types";

/**
 * The pure parts of the morning review (task 017). jsdom has no layout engine,
 * so everything a test can assert exactly lives here rather than in a
 * component.
 *
 * What the review view does **not** hold: which runs count as "last night",
 * what order the digest leads in, or what its totals are. Those are rules, and
 * rules live in `rimaia-core` (ADR-0006) so the MCP tool and this screen cannot
 * disagree. Everything below formats what `get_review_digest` returned.
 */

/** One word per outcome (ADR-0024 point 3: a dot and a word). */
export const DIGEST_OUTCOME_LABELS: Record<DigestOutcome, string> = {
  failed: "Failed",
  blocked: "Blocked",
  waiting_retry: "Waiting for retry",
  interrupted: "Interrupted",
  cancelled: "Cancelled",
  running: "Running",
  completed: "Completed",
  skipped: "Skipped",
};

/**
 * Where the app opens, decided before the first frame.
 *
 * Keyed on `totals.runs`, not on "the digest has entries": Blocked and Skipped
 * entries come from `ready` tasks whatever the window, so a board with one
 * blocked chain would otherwise open on the review view at every launch, and
 * the digest marker moves only when a review empties `in_review` — nothing
 * could ever clear it. `totals.runs` is exactly what the marker empties.
 *
 * `digest` is `null` when the read failed: a failed read is not a reason to
 * withhold the app, the same fallback `App` already makes for `get_app_info`.
 */
export function openingView(onboardingDismissed: boolean, digest: ReviewDigest | null): View {
  if (!onboardingDismissed) return "welcome";
  if (digest !== null && digest.totals.runs > 0) return "review";
  return "board";
}

/**
 * The review queue: `in_review` tasks in **board order**, archived ones
 * excluded. `groupIntoColumns` is the one place that order is defined —
 * `position` is only comparable within one (repository, column) (ADR-0007), so
 * a global sort by it would interleave repositories and disagree with the board
 * this view sits beside.
 */
export function reviewQueue<T extends BoardCard & { archivedAt: string | null }>(
  tasks: readonly T[],
): T[] {
  return [...groupIntoColumns(tasks.filter((task) => task.archivedAt === null)).in_review];
}

/**
 * Which task to show once `id` is gone from `previousIds`: the one that
 * followed it and is still in the queue, else the nearest one before it, else
 * nothing. Used for the view's own approve and for a task another door moved.
 */
export function successor(
  previousIds: readonly string[],
  id: string,
  remainingIds: readonly string[],
): string | null {
  const remaining = new Set(remainingIds);
  const index = previousIds.indexOf(id);
  if (index === -1) return remainingIds[0] ?? null;
  for (let next = index + 1; next < previousIds.length; next += 1) {
    if (remaining.has(previousIds[next])) return previousIds[next];
  }
  for (let before = index - 1; before >= 0; before -= 1) {
    if (remaining.has(previousIds[before])) return previousIds[before];
  }
  return null;
}

export type ReviewMode = "digest" | "queue";

export type ReviewCommand =
  | "start_review"
  | "show_digest"
  | "next"
  | "previous"
  | "approve"
  | "reject"
  | "request_changes"
  | "open_pull_request"
  | "open_worktree";

/** The keys, as data: the legend renders from this table, so it cannot list a
 *  key the handler does not bind. */
export const REVIEW_KEYS: ReadonlyArray<{
  readonly mode: ReviewMode;
  readonly keys: readonly string[];
  readonly label: string;
  readonly command: ReviewCommand;
}> = [
  { mode: "digest", keys: ["Enter"], label: "start the review", command: "start_review" },
  { mode: "queue", keys: ["j", "→"], label: "next", command: "next" },
  { mode: "queue", keys: ["k", "←"], label: "previous", command: "previous" },
  { mode: "queue", keys: ["a"], label: "approve", command: "approve" },
  { mode: "queue", keys: ["r"], label: "reject", command: "reject" },
  { mode: "queue", keys: ["c"], label: "needs changes", command: "request_changes" },
  { mode: "queue", keys: ["o"], label: "open the PR", command: "open_pull_request" },
  { mode: "queue", keys: ["w"], label: "open the worktree", command: "open_worktree" },
  { mode: "queue", keys: ["d"], label: "digest", command: "show_digest" },
];

const KEY_COMMANDS: Record<ReviewMode, Record<string, ReviewCommand>> = {
  digest: { Enter: "start_review" },
  queue: {
    j: "next",
    ArrowRight: "next",
    k: "previous",
    ArrowLeft: "previous",
    a: "approve",
    r: "reject",
    c: "request_changes",
    o: "open_pull_request",
    w: "open_worktree",
    d: "show_digest",
  },
};

/**
 * What a keypress means in a mode, or `null`. A chord is never a shortcut:
 * Cmd+R is the browser's, and Mod+Enter belongs to the note field.
 */
export function reviewCommandForKey(
  mode: ReviewMode,
  event: Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "altKey">,
): ReviewCommand | null {
  if (event.ctrlKey || event.metaKey || event.altKey) return null;
  return KEY_COMMANDS[mode][event.key] ?? null;
}

const RUN_STATUS_LABELS: Record<RunStatus, string> = {
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
};

export type OutcomeTone = "running" | "success" | "failed" | "cancelled";

/**
 * The newest run's outcome as a word. "Interrupted" is read off the run's own
 * exit class and never off the task's `run_state` (seam-contract D9): a task
 * whose run was interrupted reads `failed`, and the word would be wrong.
 */
export function runOutcome(run: { status: RunStatus; exitClass: ExitClass | null }): {
  label: string;
  tone: OutcomeTone;
} {
  if (run.exitClass === "interrupted") return { label: "Interrupted", tone: "failed" };
  const label = RUN_STATUS_LABELS[run.status];
  switch (run.status) {
    case "running":
      return { label, tone: "running" };
    case "succeeded":
      return { label, tone: "success" };
    case "cancelled":
      return { label, tone: "cancelled" };
    default:
      return { label, tone: "failed" };
  }
}

/** `3s`, `12m 34s`, `1h 12m` — the same shape the run overlay uses, with hours
 *  for the night's totals. */
export function formatSeconds(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  if (hours > 0) return `${hours}h ${minutes}m`;
  if (minutes > 0) return `${minutes}m ${total % 60}s`;
  return `${total}s`;
}

/** Whole cents for a figure that has them, four places for the sub-cent runs a
 *  two-decimal format would erase (the same reason `formatCostUsd` exists). */
export function formatDigestCost(costUsd: number): string {
  return costUsd >= 0.01 || costUsd === 0 ? `$${costUsd.toFixed(2)}` : `$${costUsd.toFixed(4)}`;
}

/** What an entry's run figures read as. Only a task with a run that ended in
 *  the window has figures at all; `null` there is "not recorded" (D18), never
 *  zero. A blocked or skipped entry has no run, so there is nothing to record. */
export function entryFigures(entry: DigestEntry): { duration: string; cost: string } | null {
  if (entry.runs === 0) return null;
  return {
    duration: entry.runSeconds === null ? "not recorded" : formatSeconds(entry.runSeconds),
    cost: entry.costUsd === null ? "not recorded" : formatDigestCost(entry.costUsd),
  };
}

/** Why a digest entry is where it is, as one phrase, or `null`. */
export function entryReason(entry: DigestEntry): string | null {
  if (entry.outcome === "blocked" && entry.blockingTitle !== null) {
    return `waiting on ${entry.blockingTitle}`;
  }
  if (entry.outcome === "skipped" && entry.skipReason !== null) {
    return QUEUE_SKIP_LABELS[entry.skipReason];
  }
  return null;
}

/**
 * The totals line. `totals.cost_usd` sums only runs that recorded a cost, so
 * when none did the figure would be a $0.00 nobody measured; say so instead.
 */
function totalsSentence(digest: ReviewDigest): string {
  const { totals } = digest;
  const everyCostMissing = totals.runs > 0 && totals.runsWithoutCost === totals.runs;
  const cost = everyCostMissing ? "cost not recorded" : formatDigestCost(totals.costUsd);
  const missing =
    totals.runsWithoutCost > 0 && !everyCostMissing
      ? ` (${totals.runsWithoutCost} ${totals.runsWithoutCost === 1 ? "run" : "runs"} had no recorded cost)`
      : "";
  return `${totals.runs} ${totals.runs === 1 ? "run" : "runs"}, ${formatSeconds(
    totals.runSeconds,
  )} run time, ${cost}${missing}`;
}

/** The outcome counts the service returned, non-zero only, in digest order. */
export function outcomeCounts(digest: ReviewDigest): Array<{ outcome: DigestOutcome; count: number }> {
  return (Object.keys(DIGEST_OUTCOME_LABELS) as DigestOutcome[])
    .map((outcome) => ({ outcome, count: digest.totals.counts[outcome] }))
    .filter(({ count }) => count > 0);
}

/**
 * The digest as plain text, for pasting into a message. Built from the same
 * data the screen renders, in the order the service returned it — no sorting
 * and no arithmetic over entries.
 */
export function digestAsText(digest: ReviewDigest, now: Date): string {
  const lines = [`Rimaia overnight digest, since ${relativeTime(digest.since, now)}`];
  if (digest.entries.length === 0 && digest.totals.runs === 0) {
    lines.push("Nothing has ended since the last finished review.");
    return lines.join("\n");
  }
  lines.push(totalsSentence(digest));
  if (digest.totals.spanSeconds !== null) {
    lines.push(`Wall-clock span ${formatSeconds(digest.totals.spanSeconds)}`);
  }
  lines.push(
    outcomeCounts(digest)
      .map(({ outcome, count }) => `${count} ${DIGEST_OUTCOME_LABELS[outcome].toLowerCase()}`)
      .join(", "),
  );
  lines.push("");
  for (const entry of digest.entries) {
    const details: string[] = [];
    const reason = entryReason(entry);
    if (reason) details.push(reason);
    const figures = entryFigures(entry);
    if (figures) {
      details.push(
        figures.duration === "not recorded" ? "duration not recorded" : figures.duration,
        figures.cost === "not recorded" ? "cost not recorded" : figures.cost,
      );
    }
    lines.push(
      `- ${DIGEST_OUTCOME_LABELS[entry.outcome]}: ${entry.title}${
        details.length > 0 ? ` (${details.join(", ")})` : ""
      }`,
    );
  }
  return lines.join("\n");
}

/** The dependents a verdict would block: ADR-0008's amendment makes a
 *  dependency satisfied only in `in_review` or `done`, so both reject and
 *  needs-changes leave every dependent blocked — except an archived one, which
 *  is never picked up and so is blocked by nothing. */
export function affectedDependents(dependents: readonly TaskDependent[]): TaskDependent[] {
  return dependents.filter((dependent) => dependent.archivedAt === null);
}

/** One line for a task another door took out of the queue. `current` is the
 *  board's present row for it, or `undefined` when it has left the board. */
export function describeDeparture(
  title: string,
  current: { column: keyof typeof COLUMN_TITLES } | undefined,
): string {
  if (current === undefined) return `“${title}” is no longer on the board.`;
  return `“${title}” was moved to ${COLUMN_TITLES[current.column]} from outside this view.`;
}
