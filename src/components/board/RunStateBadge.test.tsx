import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RunStateBadge } from "./RunStateBadge";
import type { RunState } from "../../types";

describe("RunStateBadge", () => {
  it("renders nothing for an idle task", () => {
    const { container } = render(<RunStateBadge runState="idle" lastRun={null} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("renders a distinctly classed badge for every other run state", () => {
    const states: RunState[] = ["running", "queued", "blocked", "waiting_retry", "cancelled"];

    for (const state of states) {
      const { unmount } = render(<RunStateBadge runState={state} lastRun={null} />);
      const badge = screen.getByText(new RegExp(state === "waiting_retry" ? "retry" : state, "i"));
      expect(badge.className).toContain(`run-badge-${state}`);
      unmount();
    }
  });

  it("shows the animated running badge, not a generic one", () => {
    render(<RunStateBadge runState="running" lastRun={null} />);
    expect(screen.getByText("Running").className).toContain("run-badge-running");
  });

  it("reads interrupted off the last run for a failed task (D9), not a bare failed badge", () => {
    render(<RunStateBadge runState="failed" lastRun={{ exitClass: "interrupted" }} />);
    const badge = screen.getByText("Interrupted");
    expect(badge.className).toContain("run-badge-interrupted");
    expect(badge.className).not.toContain("run-badge-failed");
  });

  it("shows failed when the last run stopped for any other reason", () => {
    render(<RunStateBadge runState="failed" lastRun={{ exitClass: "fatal" }} />);
    expect(screen.getByText("Failed").className).toContain("run-badge-failed");
  });

  it("says when a waiting task resumes, not only that it is waiting", () => {
    // Task 014's "card badge showing `waiting_retry` with the time it will
    // resume". At 09:00 the time is the whole value of the badge: a card coming
    // back at 06:12 and one whose retries ran out otherwise read identically,
    // and only one of them needs a human.
    const at = new Date(Date.now() + 4 * 60 * 60 * 1000);
    render(
      <RunStateBadge
        runState="waiting_retry"
        lastRun={{ exitClass: "usage_limit", resumeAfter: at.toISOString() }}
      />,
    );

    const badge = screen.getByText(/Waiting for retry/);
    expect(badge.className).toContain("run-badge-waiting_retry");
    expect(badge.textContent).toContain("resumes");
  });

  it("still renders a waiting badge when nothing is scheduled", () => {
    // A hand-edited row, or a starter this codebase has not met. The badge is
    // still true; there is simply no time to add to it.
    render(<RunStateBadge runState="waiting_retry" lastRun={{ exitClass: null }} />);

    const badge = screen.getByText("Waiting for retry");
    expect(badge.textContent).not.toContain("resumes");
  });
});
