import { ErrorBanner } from "../../components/ErrorBanner";
import { listRunners } from "../../lib/commands";
import { lastSeen } from "../../lib/format";
import type { RunnerEligibility } from "../../types";
import { useAccountRead } from "../account/useAccountRead";

const ELIGIBILITY_LABELS: Record<RunnerEligibility, string> = {
  assigned: "Takes tasks assigned to you",
  assigned_then_pool: "Takes tasks assigned to you, then the team pool",
};

/**
 * Settings → Runners, in a browser (task 050 Scope 6, ADR-0034 point 5 as its
 * 2026-10-04 amendment narrows it).
 *
 * **It shows no runner setting's value.** The board holds none: every one lives
 * in `runner.db` (D28 part 4), and a reported copy would be stale exactly when
 * its runner is offline. So each runner is the board's own facts about it, and
 * a sentence saying where its settings are changed. The sentence names neither
 * the desktop app nor a command line, because the board cannot tell a desktop
 * runner from a headless one (task 058).
 *
 * The doctor sentence is replaced by the runner's result when task 069
 * renders `list_runner_doctor_reports` here.
 */
export function RunnersSection({ now = new Date() }: { readonly now?: Date }) {
  const { value: runners, error } = useAccountRead(listRunners);

  return (
    <section className="panel runners-section" aria-labelledby="runners-heading">
      <h3 id="runners-heading">Runners</h3>
      <p className="muted">
        The machines that run your tasks. What each one is allowed to do is decided on that
        machine.
      </p>
      {error && <ErrorBanner error={error} />}
      {runners === null && !error && <p className="muted">Reading…</p>}
      {runners && runners.length === 0 && (
        <p className="muted">
          No runner is paired to your account. Pair one from the Account page.
        </p>
      )}
      {runners && runners.length > 0 && (
        <ul className="runner-rows">
          {runners.map((runner) => (
            <li key={runner.id} className="runner-row">
              <h4>{runner.label}</h4>
              <dl className="detail-list">
                <dt>Provider</dt>
                <dd>{runner.provider}</dd>
                <dt>Version</dt>
                <dd className="tabular-nums">
                  {runner.appVersion ? `v${runner.appVersion}` : "Not reported"}
                </dd>
                <dt>Eligibility</dt>
                <dd>{ELIGIBILITY_LABELS[runner.eligibility]}</dd>
                <dt>Last seen</dt>
                <dd className="tabular-nums">{lastSeen(runner.lastSeenAt, now)}</dd>
              </dl>
              <p className="muted">{`${runner.label}'s settings are changed on that machine, not here.`}</p>
              <p className="muted">{`${runner.label} has not reported a doctor result yet.`}</p>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
