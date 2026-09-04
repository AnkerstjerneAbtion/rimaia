import type {
  DoctorCheck,
  DoctorCheckResult,
  DoctorDismissal,
  DoctorReport,
  DoctorStatus,
} from "../types";

/**
 * Reading a doctor report, in one place.
 *
 * Every predicate here mirrors one on `rimaia_core::doctor` — deliberately, and
 * deliberately only the *reading* ones. Nothing in this file decides whether the
 * queue may start: that refusal lives on `QueueHandle::start` in Rust, so the
 * board and any future MCP caller inherit the same answer (ADR-0006). What the
 * frontend needs is narrower — which rows to show and how loudly — and
 * recomputing "any status is fail" here to *colour a banner* is presentation,
 * not a second copy of the rule.
 */

/** Worst first: a report with one failure is a failing report. */
const SEVERITY: Record<DoctorStatus, number> = { fail: 0, warn: 1, pass: 2 };

/**
 * The rows worth showing a user. A passing installation shows no banner.
 *
 * Dismissed rows drop out here — this is already the one place that decides
 * which rows the banner shows, and task 027's whole design is that dismissal is
 * *presentation*. The row is still on the report, still marked, and Settings →
 * Environment renders the full list including it.
 */
export function problems(report: DoctorReport | null): DoctorCheckResult[] {
  if (!report) return [];
  return report.results
    .filter((result) => result.status !== "pass" && !result.dismissed)
    .sort((a, b) => SEVERITY[a.status] - SEVERITY[b.status]);
}

/**
 * Whether the backend would refuse to start the queue right now.
 *
 * Deliberately **not** derived from {@link problems}, which now hides dismissed
 * rows: a dismissal must not be able to change this answer, and computing it
 * off a filtered list is exactly how it would. `fail` is not dismissible in the
 * first place, so today the two readings agree — the point is that they still
 * agree if that ever changes.
 */
export function hasBlockingProblem(report: DoctorReport | null): boolean {
  if (!report) return false;
  return report.results.some((result) => result.status === "fail");
}

/** The rows the user has put down, in the order the report lists them. */
export function dismissedProblems(report: DoctorReport | null): DoctorCheckResult[] {
  if (!report) return [];
  return report.results.filter((result) => result.dismissed);
}

/**
 * The dismissal that answers one row — its check, its repository and its exact
 * sentence, which is the whole key (`CheckResult::dismissal` in Rust).
 */
export function dismissalFor(result: DoctorCheckResult): DoctorDismissal {
  return { check: result.check, repository: result.repository, detail: result.detail };
}

/**
 * Whether `dismissal` is the one answering `result`.
 *
 * The only predicate here that mirrors a *writing* rule rather than a reading
 * one, and it exists so a dismiss or restore can update the banner without
 * re-running eight subprocesses to find out what changed. The authoritative
 * marking stays in `DoctorReport::new`, which re-marks every real report —
 * including the very next one this window reads.
 */
export function matchesDismissal(
  result: DoctorCheckResult,
  dismissal: DoctorDismissal,
): boolean {
  return (
    result.check === dismissal.check &&
    result.repository === dismissal.repository &&
    result.detail === dismissal.detail
  );
}

/** Whether a dismissal still names a row on this report. */
export function isStale(report: DoctorReport, dismissal: DoctorDismissal): boolean {
  return !report.results.some(
    (result) => result.dismissed && matchesDismissal(result, dismissal),
  );
}

/** The rows belonging to one step of the welcome flow, or one Settings panel. */
export function resultsFor(
  report: DoctorReport | null,
  checks: readonly DoctorCheck[],
): DoctorCheckResult[] {
  if (!report) return [];
  return report.results.filter((result) => checks.includes(result.check));
}

/**
 * The worst status among `results`, or `null` when there are none.
 *
 * `null` is not "pass": a welcome step whose checks have not run yet has not
 * been *done*, and rendering it as done would be the click-counting the flow is
 * built to avoid.
 */
export function worstStatus(results: DoctorCheckResult[]): DoctorStatus | null {
  if (results.length === 0) return null;
  return results.reduce<DoctorStatus>(
    (worst, result) => (SEVERITY[result.status] < SEVERITY[worst] ? result.status : worst),
    "pass",
  );
}

/** What a status is called on screen. */
export function statusLabel(status: DoctorStatus): string {
  return status === "pass" ? "OK" : status === "warn" ? "Warning" : "Blocked";
}
