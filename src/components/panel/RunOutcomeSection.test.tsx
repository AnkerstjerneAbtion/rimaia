import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { RunOutcomeSection, formatCostUsd } from "./RunOutcomeSection";
import type { ExitClass, Run } from "../../types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

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
    kind: "implementation",
    status: "succeeded",
    sessionId: "session-1",
    prompt: "prompt",
    startedAt: "2026-08-20T09:00:00Z",
    endedAt: "2026-08-20T09:10:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 5,
    costUsd: 0.1502925,
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
    headSha: null,
    baseSha: null,
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

  it("fetches the log path on the copy action, then copies and shows it", async () => {
    // `Run` carries no path since task 066: it is this computer's file,
    // derived from the run's ids by `get_run_log_path`.
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_run_log_path") {
        expect(args).toEqual({ taskId: "task-1", runId: "run-1" });
        return "/data/runs/task-1/run-1.jsonl";
      }
      throw new Error(`unexpected command: ${command}`);
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<RunOutcomeSection lastRun={run()} loading={false} />);
    expect(screen.queryByText("/data/runs/task-1/run-1.jsonl")).not.toBeInTheDocument();
    screen.getByRole("button", { name: "Copy log path" }).click();

    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
    expect(writeText).toHaveBeenCalledWith("/data/runs/task-1/run-1.jsonl");
    expect(screen.getByText("/data/runs/task-1/run-1.jsonl")).toBeInTheDocument();
  });

  it("shows the refusal when this computer cannot say where the log is", async () => {
    mockInvoke.mockRejectedValue({
      code: "invalid",
      message: '"rimaia" is not set up on this computer',
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.assign(navigator, { clipboard: { writeText } });

    render(<RunOutcomeSection lastRun={run()} loading={false} />);
    screen.getByRole("button", { name: "Copy log path" }).click();

    expect(
      await screen.findByText('"rimaia" is not set up on this computer'),
    ).toBeInTheDocument();
    expect(writeText).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Copy log path" })).toBeInTheDocument();
  });

  it("shows the turn count", () => {
    render(<RunOutcomeSection lastRun={run({ numTurns: 7 })} loading={false} />);

    expect(screen.getByText("7")).toBeInTheDocument();
  });

  it("names the row in its heading, so a review does not read as the implementation", () => {
    render(<RunOutcomeSection lastRun={run({ kind: "review", attempt: 7 })} loading={false} />);

    expect(
      screen.getByRole("heading", { name: "Last run outcome — Review · #7" }),
    ).toBeInTheDocument();
  });

  it("keeps a plain heading while there is nothing to name", () => {
    render(<RunOutcomeSection lastRun={null} loading={false} />);

    expect(screen.getByRole("heading", { name: "Last run outcome" })).toBeInTheDocument();
  });
});
