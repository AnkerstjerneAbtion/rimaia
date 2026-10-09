import { QUEUE_SKIP_LABELS } from "../components/runs/QueuePlanList";
import { COLUMN_TITLES } from "../components/board/Column";
import { groupIntoColumns, relativeTime } from "./board";
import type { BoardCard } from "./board";
import type {
  DigestEntry,
  DigestOutcome,
  ExitClass,
  HistoryFinding,
  LoopHistory,
  ReviewDigest,
  ReviewHistory,
  RunCostSummary,
  RunState,
  RunStatus,
  TaskDependent,
  UnreviewedReason,
  Verdict,
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

// ---------------------------------------------------------------------------
// The review loop, in words (task 037)
//
// Everything here formats what core decided. Which findings block, how many are
// open, whether the loop may be going in circles and what a verdict is are
// `rimaia-core`'s (ADR-0017); a function in this section that compared two
// severities or counted a list of findings would be a second copy of a rule.
// ---------------------------------------------------------------------------

const UNREVIEWED_TEXT: Record<UnreviewedReason, string> = {
  not_reviewed: "Not reviewed",
  review_failed: "Not reviewed — the review run failed",
  nothing_recorded: "Not reviewed — the reviewer recorded nothing",
  review_changed_branch: "Not reviewed — the reviewer changed the branch",
  fix_not_reviewed: "Not reviewed since the last fix",
};

/** The card's second element when core flags a ping-pong. Core's wording: a
 *  fresh reviewer may simply have noticed something the first one missed, so
 *  this must not claim the fix broke anything. */
export const PING_PONG_TEXT = "May be going in circles";

function plural(count: number, one: string, many: string): string {
  return `${count} ${count === 1 ? one : many}`;
}

/** `Reviewed once` or `Reviewed after 2 fixes`: the prefix counts fixes,
 *  because `max_review_loops` counts fixes (task 021). */
function reviewedPrefix(fixesSpent: number): string {
  return fixesSpent === 0
    ? "Reviewed once"
    : `Reviewed after ${plural(fixesSpent, "fix", "fixes")}`;
}

/** What the loop line reads of a loop: `ReviewLoopSummary` and the history's
 *  `LoopHistory` both satisfy it, so the card, the panel and the overlay say it
 *  in the same words. */
export interface LoopWords {
  readonly fixesSpent: number;
  readonly verdict: Verdict;
}

/**
 * The card's loop line, or `null` when the loop has nothing to say.
 *
 * There is no tick, no "clean" and no "passed": a loop with nothing blocking is
 * a statement about one reviewer's pass, not a verdict on the work
 * (ADR-0017 asks for "loop count and findings history rather than a green
 * tick").
 */
export function reviewLoopText(summary: LoopWords | null | undefined): string | null {
  if (!summary) return null;
  const verdict: Verdict = summary.verdict;
  switch (verdict.verdict) {
    case "none":
      return null;
    case "clean":
      return `${reviewedPrefix(summary.fixesSpent)} · nothing blocking`;
    case "findings_remain":
      return `${reviewedPrefix(summary.fixesSpent)} · ${plural(
        verdict.openBlocking,
        "blocking finding",
        "blocking findings",
      )} open`;
    case "unreviewed":
      return UNREVIEWED_TEXT[verdict.reason];
  }
}

/**
 * The loop a card may speak about: none while the task is still moving.
 *
 * The rule is on `runState`, not on the badge's words. A verdict on a loop in
 * progress describes an unfinished pass, and the badge already says what is
 * happening: an implementation re-run, a review or fix in flight, and the
 * window between phases (where `runState` is still `running` and the newest
 * row is a finished review) all read the same here.
 */
export function finishedLoop<T extends LoopWords>(
  runState: RunState,
  summary: T | null | undefined,
): T | null {
  if (runState === "queued" || runState === "running" || runState === "waiting_retry") {
    return null;
  }
  return summary ?? null;
}

/** What `reviewHistoryHeadText` reads of a loop: core's verdict and counts. */
export interface LoopHead {
  readonly fixesSpent: number;
  readonly verdict: Verdict;
  readonly openBlocking: number;
  readonly openAdvisory: number;
}

/**
 * The head of the task panel's history: what remains in the current loop,
 * from core's counts and never from counting findings here. `null` when the
 * loop has no verdict.
 */
export function reviewHistoryHeadText(loop: LoopHead): string | null {
  const { verdict } = loop;
  switch (verdict.verdict) {
    case "none":
      return null;
    case "unreviewed":
      return UNREVIEWED_TEXT[verdict.reason];
    case "clean":
    case "findings_remain": {
      const advisory = loop.openAdvisory;
      let open: string;
      if (loop.openBlocking > 0) {
        open = `${plural(loop.openBlocking, "blocking finding", "blocking findings")} open`;
        if (advisory > 0) open += `, ${advisory} advisory`;
      } else {
        open = advisory > 0 ? `nothing blocking, ${advisory} advisory` : "nothing open";
      }
      return `${reviewedPrefix(loop.fixesSpent)} · ${open}`;
    }
  }
}

/**
 * What turning the loop on costs, in the words of this installation's own
 * median run, or what it cannot say yet.
 *
 * `maxReviewLoops` is the value that will be **effective** at the level being
 * edited once the loop is on. With N fix loops a task runs at most `2N + 1`
 * more sessions before retries: one review, then a fix and another review per
 * loop (task 021). The sentence never states a figure it did not measure, which
 * is `environmentOverheadNote`'s rule: with no finished run that reported a
 * cost it says so instead of guessing a price.
 *
 * The median is `observed_run_cost`'s, over runs of every kind (seam-contract
 * D29 point 6).
 */
export function reviewLoopCostNote(
  maxReviewLoops: number,
  summary: RunCostSummary | null,
): string {
  const sessions = 2 * maxReviewLoops + 1;
  const lead =
    maxReviewLoops === 0
      ? "With no fix loops, each task runs 1 more session: a review that reports findings and fixes nothing."
      : `With up to ${plural(maxReviewLoops, "fix loop", "fix loops")}, each task runs up to ${sessions} more sessions: a review, then a fix and another review per loop.`;

  const median = summary?.medianUsd;
  if (!summary || median == null || median <= 0) {
    return `${lead} There is no finished run with a cost yet to put a price on that.`;
  }

  const runs = plural(summary.sampleSize, "run", "runs");
  const observed = `At your median run so far ($${median.toFixed(2)} across ${runs})`;
  const total = (median * sessions).toFixed(2);
  return maxReviewLoops === 0
    ? `${lead} ${observed}, that is about $${total} more per task.`
    : `${lead} ${observed}, that is up to about $${total} more per task.`;
}

// ---------------------------------------------------------------------------
// Reading a ReviewHistory (task 037)
//
// The history is rendered as core returned it. Grouping runs into loops, what
// blocks and how many findings are open are core's; these functions only find
// things in it.
// ---------------------------------------------------------------------------

/** The loop the verdict is about: the newest, which core does not mark
 *  `earlier`. */
export function currentLoop(history: ReviewHistory): LoopHistory | null {
  return history.loops.find((loop) => !loop.earlier) ?? null;
}

/** Whether there is anything to show: at least one loop with a review or a
 *  fix in it. An implementation with no rounds is not a history. */
export function hasReviewHistory(history: ReviewHistory | null): history is ReviewHistory {
  return history !== null && history.loops.some((loop) => loop.rounds.length > 0);
}

/** Every run id in the history mapped to its `attempt`, so `Fixed in #6` can
 *  name a row. */
export function attemptsByRun(history: ReviewHistory): ReadonlyMap<string, number> {
  const attempts = new Map<string, number>();
  for (const loop of history.loops) {
    const phases = [
      loop.implementation,
      ...loop.rounds.flatMap((round) => [round.review, round.fix?.phase ?? null]),
    ];
    for (const phase of phases) {
      phase?.runIds.forEach((id, index) => attempts.set(id, phase.attempts[index]));
    }
  }
  return attempts;
}

/** The highest `attempt` the history knows: the task's newest row, whose
 *  bundle is the branch as it actually is (seam-contract D29 point 5). */
export function newestAttempt(history: ReviewHistory): number | null {
  const all = [...attemptsByRun(history).values()];
  return all.length === 0 ? null : Math.max(...all);
}

/** Blocking before advisory, otherwise as given. Reads the flag core set and
 *  never a severity. */
function blockingFirst(findings: readonly HistoryFinding[]): HistoryFinding[] {
  return [
    ...findings.filter((finding) => finding.blocking),
    ...findings.filter((finding) => !finding.blocking),
  ];
}

/** Whether `runId` is the task's newest row, whose bundle is the branch as it
 *  actually is. `null` stands for "the task as it is now", which the morning
 *  review always is. */
export function isNewestRun(history: ReviewHistory, runId: string | null): boolean {
  if (runId === null) return true;
  const attempt = attemptsByRun(history).get(runId);
  return attempt !== undefined && attempt === newestAttempt(history);
}

/** Core's ping-pong signal for a loop: any review after a fix raised a finding
 *  the fix had fixed, or a new blocking one. */
export function loopPingPong(loop: LoopHistory): boolean {
  return loop.rounds.some((round) => round.pingPong);
}

export interface FindingsForRun {
  /** `open`: what is still unresolved on the branch the run left. `raised`:
   *  what an older review found, each with its status today. */
  readonly kind: "open" | "raised";
  readonly findings: HistoryFinding[];
}

/**
 * The findings to show beside a run, or `null` when the run has none to
 * speak of.
 *
 * `runId === null` is the morning review, which is always about the task's
 * newest state. For a run, the task's newest row shows the current loop's open
 * findings, and an older review row shows what that review raised. A fix or an
 * implementation that is not the newest row shows nothing.
 */
export function findingsForRun(
  history: ReviewHistory,
  runId: string | null,
): FindingsForRun | null {
  if (isNewestRun(history, runId)) {
    const loop = currentLoop(history);
    const reviewed = loop?.rounds.filter((round) => round.review !== null) ?? [];
    const last = reviewed[reviewed.length - 1];
    if (!last) return null;
    return {
      kind: "open",
      findings: blockingFirst(last.findings.filter((finding) => finding.status === "open")),
    };
  }

  if (runId === null) return null;
  for (const loop of history.loops) {
    const round = loop.rounds.find((candidate) => candidate.review?.runIds.includes(runId));
    if (round) return { kind: "raised", findings: blockingFirst(round.findings) };
  }
  return null;
}

/**
 * A finding's status in words: `Open`, `Fixed in #5`, or
 * `Rejected in #5 — <resolution>`. A rejection that was carried over has no
 * run to name and already says why, so its stored resolution is shown
 * verbatim. A rejection's reason is always visible.
 */
export function findingStatusText(
  finding: HistoryFinding,
  attempts: ReadonlyMap<string, number>,
): string {
  if (finding.status === "open") return "Open";
  if (finding.carriedOver) return finding.resolution ?? "Rejected earlier";
  const attempt = finding.resolvedByRunId ? attempts.get(finding.resolvedByRunId) : undefined;
  const where = attempt === undefined ? "" : ` in #${attempt}`;
  if (finding.status === "fixed") return `Fixed${where}`;
  return finding.resolution ? `Rejected${where} — ${finding.resolution}` : `Rejected${where}`;
}
