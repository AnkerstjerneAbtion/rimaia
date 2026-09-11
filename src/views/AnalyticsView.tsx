import { useCallback, useEffect, useMemo, useState } from "react";

import { ErrorBanner } from "../components/ErrorBanner";
import {
  getAnalytics,
  getSubscriptionCost,
  setSubscriptionCost,
  toRimaiaError,
} from "../lib/commands";
import type { Analytics, RimaiaError } from "../types";

/**
 * What the queue has actually done (task 024, ADR-0022).
 *
 * Read-only. Every figure is computed in Rust from `runs` at read time; nothing
 * on this page is stored, and nothing on it writes a row — except the one field
 * that is not a figure at all, the subscription cost, which is the user's own
 * statement about their billing.
 *
 * # Three rules the page is wrong without
 *
 * **A NULL is never averaged as a zero.** A period that predates ADR-0022's
 * capture columns is *partly unrecorded*, and this says so in a line above the
 * total rather than quoting a smaller number as if it were the whole one
 * (seam-contract D18).
 *
 * **Cost per completed task divides total spend by completed tasks**, counting
 * every failed attempt. The flattering version — the cost of the successful run
 * — hides the thing worth knowing.
 *
 * **The subscription comparison is absent, not zero**, until the user enters a
 * figure, and it is labelled as theirs because Rimaia cannot verify it.
 *
 * # No charting dependency
 *
 * The one chart is a row of `<div>`s with a height, which is what task 024's
 * Out of scope asks for: "a bar chart is not worth a bundle".
 */

type PeriodId = "week" | "month" | "all";

const PERIODS: { id: PeriodId; label: string }[] = [
  { id: "week", label: "Last 7 days" },
  { id: "month", label: "This month" },
  { id: "all", label: "All time" },
];

/**
 * The window's own calendar, deliberately — the bounds are resolved here rather
 * than in Rust because "this month" is a question about the *user's* timezone,
 * and core has no business having an opinion about it.
 */
function boundsFor(period: PeriodId, now: Date): { from: Date | null; to: Date | null } {
  if (period === "all") return { from: null, to: null };
  if (period === "week") {
    const from = new Date(now);
    from.setDate(from.getDate() - 7);
    return { from, to: null };
  }
  return { from: new Date(now.getFullYear(), now.getMonth(), 1), to: null };
}

function money(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

function duration(seconds: number): string {
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)}m`;
  return `${(seconds / 3600).toFixed(1)}h`;
}

function percent(fraction: number): string {
  return `${Math.round(fraction * 100)}%`;
}

export function AnalyticsView() {
  const [period, setPeriod] = useState<PeriodId>("month");
  const [report, setReport] = useState<Analytics | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [loading, setLoading] = useState(true);
  const [subscriptionDraft, setSubscriptionDraft] = useState("");

  const refresh = useCallback(async (selected: PeriodId) => {
    setLoading(true);
    const { from, to } = boundsFor(selected, new Date());
    try {
      const next = await getAnalytics(from, to);
      setReport(next);
      setError(null);
      setSubscriptionDraft(
        next.subscriptionMonthlyUsd === null ? "" : String(next.subscriptionMonthlyUsd),
      );
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh(period);
  }, [refresh, period]);

  // Read once on mount as well: the page can be opened before any run exists,
  // and the field should still show a figure the user already entered.
  useEffect(() => {
    getSubscriptionCost().then(
      (value) => setSubscriptionDraft(value === null ? "" : String(value)),
      () => undefined,
    );
  }, []);

  const busiestDay = useMemo(
    () =>
      report?.spendByDay.reduce<number>((most, day) => Math.max(most, day.spendUsd), 0) ?? 0,
    [report],
  );

  async function saveSubscription() {
    const trimmed = subscriptionDraft.trim();
    try {
      await setSubscriptionCost(trimmed === "" ? null : Number(trimmed));
      await refresh(period);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    }
  }

  return (
    <div className="view analytics-view">
      <header className="view-header">
        <h2>Analytics</h2>
        <p className="muted">
          What the queue has actually done. Every figure is computed from the run history when
          this page opens — nothing here is stored or rolled up.
        </p>
      </header>

      <div className="analytics-periods" role="group" aria-label="Period">
        {PERIODS.map((option) => (
          <button
            key={option.id}
            type="button"
            aria-pressed={period === option.id}
            onClick={() => setPeriod(option.id)}
          >
            {option.label}
          </button>
        ))}
      </div>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {report === null && loading && <p className="muted">Reading the run history…</p>}

      {report && (
        <>
          {/* D18, rendered. A period that predates the capture columns is not a
              cheaper period, and saying nothing here would make the total a
              claim about history that is false. */}
          {report.runsWithoutCost > 0 && (
            <p className="analytics-unrecorded" role="note">
              {report.runsWithoutCost} of {totalRuns(report)} runs in this period recorded
              no cost — from before Rimaia captured it, or from a run that ended without
              reporting. The totals below cover the rest, and are lower than what this period
              actually cost.
            </p>
          )}

          <section className="analytics-figures">
            <Figure label="Spent" value={money(report.spendUsd)} />
            <Figure
              label="Cost per completed task"
              value={
                report.costPerCompletedTaskUsd === null
                  ? "—"
                  : money(report.costPerCompletedTaskUsd)
              }
              note="Every attempt counted, including the ones that failed"
            />
            <Figure
              label="Tasks completed"
              value={`${report.tasksCompleted} of ${report.tasksAttempted}`}
              note="Reached review or done, against attempted"
            />
            <Figure
              label="Failure rate"
              value={failureRate(report)}
              note={`${report.outcomes.failed} failed of ${finished(report)} finished`}
            />
            <Figure
              label="Median run"
              value={
                report.medianDurationSeconds === null
                  ? "—"
                  : duration(report.medianDurationSeconds)
              }
              note={
                report.longestRun
                  ? `Longest: ${duration(report.longestRun.seconds)} — ${report.longestRun.title}`
                  : undefined
              }
            />
            <Figure
              label="Unattended hours"
              value={report.unattendedHours.toFixed(1)}
              note="Summed run time; parallel runs each count"
            />
          </section>

          {/* Layout and a little SVG-free markup, per task 024's Out of scope:
              a bar chart is not worth a bundle. */}
          {report.spendByDay.length > 0 && (
            <section className="panel">
              <h3>Spend per day</h3>
              <ul className="analytics-bars">
                {report.spendByDay.map((day) => (
                  <li key={day.day} title={`${day.day}: ${money(day.spendUsd)} over ${day.runs} runs`}>
                    <span
                      className="analytics-bar"
                      style={{
                        height: `${busiestDay > 0 ? Math.max(2, (day.spendUsd / busiestDay) * 100) : 2}%`,
                      }}
                    />
                    <span className="analytics-bar-label">{day.day.slice(5)}</span>
                  </li>
                ))}
              </ul>
            </section>
          )}

          <section className="panel">
            <h3>Model mix</h3>
            {/* By count *and* by spend, because the two rank differently and
                that difference is the interesting part. */}
            {report.models.length === 0 ? (
              <p className="muted">No run in this period recorded which model it used.</p>
            ) : (
              <table className="analytics-table">
                <thead>
                  <tr>
                    <th scope="col">Model</th>
                    <th scope="col">Runs</th>
                    <th scope="col">Spend</th>
                  </tr>
                </thead>
                <tbody>
                  {report.models.map((model) => (
                    <tr key={model.model}>
                      <td>{model.model}</td>
                      <td className="tabular-nums">{model.runs}</td>
                      <td className="tabular-nums">{money(model.spendUsd)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
            {report.runsWithoutModel > 0 && (
              <p className="muted">
                {report.runsWithoutModel} run{report.runsWithoutModel === 1 ? "" : "s"} recorded no
                model, and {report.runsWithoutModel === 1 ? "is" : "are"} not in the table above.
              </p>
            )}
          </section>

          <section className="panel">
            <h3>Deciding against doing</h3>
            <p className="muted">
              Planners cost {money(report.plannerSpendUsd)}; the runs they decided for cost{" "}
              {money(report.implementationSpendUsd)}.
            </p>
            {report.strategies.length > 0 && (
              <table className="analytics-table">
                <thead>
                  <tr>
                    <th scope="col">Strategy</th>
                    <th scope="col">Runs</th>
                    <th scope="col">Spend</th>
                  </tr>
                </thead>
                <tbody>
                  {report.strategies.map((entry) => (
                    <tr key={entry.mode}>
                      <td>{entry.mode}</td>
                      <td className="tabular-nums">{entry.runs}</td>
                      <td className="tabular-nums">{money(entry.spendUsd)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>

          <section className="panel">
            <h3>Against your subscription</h3>
            <p className="muted">
              What you pay Anthropic each month. Rimaia cannot see your bill, so this is your
              own figure — the comparison is not drawn until you enter one.
            </p>
            <div className="analytics-subscription">
              <label>
                <span className="analytics-subscription-label">Monthly cost (USD)</span>
                <input
                  type="number"
                  min="0"
                  step="1"
                  value={subscriptionDraft}
                  onChange={(event) => setSubscriptionDraft(event.target.value)}
                  placeholder="not set"
                />
              </label>
              <button type="button" onClick={() => void saveSubscription()}>
                Save
              </button>
            </div>
            {report.subscriptionMonthlyUsd !== null && report.subscriptionMonthlyUsd > 0 && (
              <p>
                This period's {money(report.spendUsd)} is{" "}
                {percent(report.spendUsd / report.subscriptionMonthlyUsd)} of the{" "}
                {money(report.subscriptionMonthlyUsd)} you said you pay each month.
              </p>
            )}
          </section>
        </>
      )}
    </div>
  );
}

function totalRuns(report: Analytics): number {
  return finished(report) + report.outcomes.running;
}

function finished(report: Analytics): number {
  const { succeeded, failed, cancelled, interrupted } = report.outcomes;
  return succeeded + failed + cancelled + interrupted;
}

/**
 * The rate the page shows, derived here rather than sent, because it is
 * *presentation* — the same reading `src/lib/doctor.ts` licenses. The rule it
 * mirrors is the one that matters: a run still going is not in the denominator,
 * or the rate would drift down every time one started.
 */
function failureRate(report: Analytics): string {
  const total = finished(report);
  return total === 0 ? "—" : percent(report.outcomes.failed / total);
}

function Figure({
  label,
  value,
  note,
}: {
  label: string;
  value: string;
  note?: string;
}) {
  return (
    <div className="analytics-figure">
      <span className="analytics-figure-label">{label}</span>
      <span className="analytics-figure-value tabular-nums">{value}</span>
      {note && <span className="analytics-figure-note">{note}</span>}
    </div>
  );
}
