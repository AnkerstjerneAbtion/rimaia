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
});
