import { useState } from "react";

import { DoctorResultList } from "./DoctorResultList";
import { hasBlockingProblem, problems } from "../lib/doctor";
import type { DoctorCheckResult, DoctorReport } from "../types";

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
 *
 * # Two ways to make it smaller, and they are not the same one (task 027)
 *
 * A `warn` is **dismissed**, one row at a time: it is by construction the case
 * where the queue can still do its job, so it is a decision the user makes once
 * and a permanent band restating it only teaches them to stop reading the
 * channel — including the day it turns into a `fail`.
 *
 * A `fail` is **collapsed**, never dismissed. The rows fold away and the
 * headline stays, naming how many there are, because the vertical space is the
 * real complaint and the sentence explaining why tonight's queue will refuse to
 * start is not. Collapsing is this component's own state and does not outlive
 * the window: a blocking environment gets to say so again next launch.
 */
export function DoctorBanner({
  report,
  onOpenSettings,
  onDismiss,
}: {
  report: DoctorReport | null;
  onOpenSettings?: () => void;
  /** Absent on a surface that only reports, like the welcome flow's steps. */
  onDismiss?: (result: DoctorCheckResult) => void;
}) {
  const [collapsed, setCollapsed] = useState(false);

  const rows = problems(report);
  const blocking = hasBlockingProblem(report);
  const blockingCount = rows.filter((result) => result.status === "fail").length;

  if (rows.length === 0) return null;

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
      {/* Only a blocking banner collapses. A warn-only one is answered row by
          row, and offering both would be two controls for one complaint. */}
      {blocking && (
        <button
          type="button"
          className="doctor-banner-collapse"
          aria-expanded={!collapsed}
          onClick={() => setCollapsed((previous) => !previous)}
        >
          {collapsed ? "Show" : "Hide"} {blockingCount} blocking{" "}
          {blockingCount === 1 ? "problem" : "problems"}
        </button>
      )}
      {!collapsed && <DoctorResultList results={rows} onDismiss={onDismiss} />}
      {onOpenSettings && (
        <button type="button" onClick={onOpenSettings}>
          Open Settings
        </button>
      )}
    </aside>
  );
}
