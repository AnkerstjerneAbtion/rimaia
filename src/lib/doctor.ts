import type { DoctorCheck, DoctorCheckResult, DoctorReport, DoctorStatus } from "../types";

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

/** The rows worth showing a user. A passing installation shows no banner. */
export function problems(report: DoctorReport | null): DoctorCheckResult[] {
  if (!report) return [];
  return report.results
    .filter((result) => result.status !== "pass")
    .sort((a, b) => SEVERITY[a.status] - SEVERITY[b.status]);
}

/** Whether the backend would refuse to start the queue right now. */
export function hasBlockingProblem(report: DoctorReport | null): boolean {
  return problems(report).some((result) => result.status === "fail");
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
