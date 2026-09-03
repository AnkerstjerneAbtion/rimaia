import { DoctorResultList } from "./DoctorResultList";
import { hasBlockingProblem, problems } from "../lib/doctor";
import type { DoctorReport } from "../types";

/**
 * The standing "your environment will not survive tonight" banner (task 018).
 *
 * Shown above every view, not only on Settings, because the failure it exists
 * to prevent happens while nobody is looking: a queue that will not start at
 * 2am is worth interrupting the board for at 6pm. A passing installation
 * renders nothing at all — {@link problems} drops the passing rows, and an
 * empty list means no banner rather than a green one.
 *
 * It states that the queue is blocked; it does not *do* the blocking. That
 * refusal is `QueueHandle::start`'s, so the MCP server and any future caller
 * inherit it (ADR-0006) — this is the same fact rendered, not a second copy of
 * the rule.
 */
export function DoctorBanner({
  report,
  onOpenSettings,
}: {
  report: DoctorReport | null;
  onOpenSettings?: () => void;
}) {
  const rows = problems(report);
  if (rows.length === 0) return null;

  const blocking = hasBlockingProblem(report);

  return (
    <aside
      className={`doctor-banner doctor-banner-${blocking ? "fail" : "warn"}`}
      role={blocking ? "alert" : "status"}
      aria-label="Environment check"
    >
      <h3>
        {blocking
          ? "The queue cannot start until these are fixed"
          : "The queue can start, but these will limit it"}
      </h3>
      <DoctorResultList results={rows} />
      {onOpenSettings && (
        <button type="button" onClick={onOpenSettings}>
          Open Settings
        </button>
      )}
    </aside>
  );
}
