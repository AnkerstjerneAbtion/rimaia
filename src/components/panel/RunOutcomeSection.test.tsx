import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RunOutcomeSection, formatCostUsd } from "./RunOutcomeSection";
import type { ExitClass, Run } from "../../types";

/**
 * The expected wording for each ADR-0011 exit class, spelled out here rather
 * than read back from `RunOutcomeSection`'s own exported `EXIT_CLASS_LABELS`
 * — importing the same mutable map the component renders from would make the
 * parametrized test below tautological: a wrong label in production would
 * relabel the assertion right along with it, and the test would still pass.
 */
const EXPECTED_EXIT_CLASS_LABELS: Record<ExitClass, string> = {
  success: "Succeeded",
  usage_limit: "Stopped — usage limit reached",
  transient: "Stopped — transient error",
  interrupted: "Interrupted",
  fatal: "Failed",
  cancelled: "Cancelled",
};

function run(overrides: Partial<Run> = {}): Run {
  return {
    id: "run-1",
    taskId: "task-1",
    attempt: 1,
    status: "succeeded",
    sessionId: "session-1",
    prompt: "prompt",
    startedAt: "2026-08-20T09:00:00Z",
    endedAt: "2026-08-20T09:10:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 5,
    costUsd: 0.1502925,
    logPath: "/data/runs/task-1/run-1.jsonl",
    prUrl: null,
    resumeAfter: null,
    baseRef: null,
    model: null,
    effort: null,
    runEnvironment: null,
    inputTokens: null,
    outputTokens: null,
    cacheReadTokens: null,
    cacheCreationTokens: null,
    ...overrides,
  };
}

describe("formatCostUsd", () => {
  it("keeps four decimal places rather than rounding the spike's own two measurements together", () => {
    // $0.1061 vs $0.0291 (spike/FINDINGS.md §2) differ starting at the third
    // decimal place — a mutation to two decimals would make both read "$0.11"
    // and "$0.03" and still pass a looser test.
    expect(formatCostUsd(0.1061)).toBe("$0.1061");
    expect(formatCostUsd(0.0291)).toBe("$0.0291");
  });

  it("rounds rather than truncates a fifth decimal place", () => {
    expect(formatCostUsd(0.1502925)).toBe("$0.1503");
  });
});

describe("RunOutcomeSection", () => {
  it("shows a loading state instead of either the empty or the resolved copy", () => {
    render(<RunOutcomeSection lastRun={null} loading />);

    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(screen.queryByText(/No runs yet/)).not.toBeInTheDocument();
  });

  it("shows the deliberate empty case for a task with no finished run", () => {
    render(<RunOutcomeSection lastRun={null} loading={false} />);

    expect(screen.getByText(/No runs yet/)).toBeInTheDocument();
  });

  // ADR-0011's six exit classes — task 008's own instruction: "the outcome
  // renders for each exit class."
  it.each(Object.keys(EXPECTED_EXIT_CLASS_LABELS) as ExitClass[])(
    "renders the label for exit class %s",
    (exitClass) => {
      render(<RunOutcomeSection lastRun={run({ exitClass })} loading={false} />);

      expect(screen.getByText(EXPECTED_EXIT_CLASS_LABELS[exitClass])).toBeInTheDocument();
    },
  );

  it("shows 'still running' rather than a blank outcome when the last run has not finished", () => {
    render(<RunOutcomeSection lastRun={run({ exitClass: null, status: "running" })} loading={false} />);

    expect(screen.getByText("Still running.")).toBeInTheDocument();
  });

  it("shows the run's cost plainly, not rounded into meaninglessness", () => {
    render(<RunOutcomeSection lastRun={run({ costUsd: 0.1061 })} loading={false} />);

    expect(screen.getByText("$0.1061")).toBeInTheDocument();
  });

  it("shows a placeholder instead of a cost figure while it is not yet known", () => {
    render(<RunOutcomeSection lastRun={run({ costUsd: null })} loading={false} />);

    expect(screen.getByText("Not available yet.")).toBeInTheDocument();
  });

  it("shows the error message for a run that failed", () => {
    render(
      <RunOutcomeSection
        lastRun={run({ exitClass: "fatal", errorMessage: "the agent reported it could not proceed" })}
        loading={false}
      />,
    );

    expect(screen.getByText("the agent reported it could not proceed")).toBeInTheDocument();
  });

  it("does not show an error row when the run carries none", () => {
    render(<RunOutcomeSection lastRun={run({ errorMessage: null })} loading={false} />);

    expect(screen.queryByText("Error")).not.toBeInTheDocument();
  });

  it("links to the pull request the agent opened", () => {
    render(
      <RunOutcomeSection
        lastRun={run({ prUrl: "https://github.com/example/rimaia/pull/9" })}
        loading={false}
      />,
    );

    const link = screen.getByRole("link", { name: "https://github.com/example/rimaia/pull/9" });
    expect(link).toHaveAttribute("href", "https://github.com/example/rimaia/pull/9");
  });

  it("does not show a pull request row when the run opened none", () => {
    render(<RunOutcomeSection lastRun={run({ prUrl: null })} loading={false} />);

    expect(screen.queryByText("Pull request")).not.toBeInTheDocument();
  });

  it("shows the log path as text", () => {
    render(<RunOutcomeSection lastRun={run({ logPath: "/data/runs/task-1/run-1.jsonl" })} loading={false} />);

    expect(screen.getByText("/data/runs/task-1/run-1.jsonl")).toBeInTheDocument();
  });

  it("copies the log path to the clipboard without a backend round trip", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<RunOutcomeSection lastRun={run({ logPath: "/data/runs/task-1/run-1.jsonl" })} loading={false} />);
    screen.getByRole("button", { name: "Copy log path" }).click();

    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(writeText).toHaveBeenCalledWith("/data/runs/task-1/run-1.jsonl");
  });

  it("shows the turn count", () => {
    render(<RunOutcomeSection lastRun={run({ numTurns: 7 })} loading={false} />);

    expect(screen.getByText("7")).toBeInTheDocument();
  });
});
