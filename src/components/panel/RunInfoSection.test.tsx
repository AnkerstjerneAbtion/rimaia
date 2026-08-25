import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { RunInfoSection } from "./RunInfoSection";
import type { Run } from "../../types";

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
    numTurns: 3,
    costUsd: 0.1,
    logPath: "/tmp/run.jsonl",
    prUrl: null,
    resumeAfter: null,
    ...overrides,
  };
}

describe("RunInfoSection", () => {
  it("renders the deliberate empty case for every field, not a blank, before task 007/008 land", () => {
    render(
      <RunInfoSection branch={null} worktreePath={null} lastRun={null} loading={false} />,
    );

    expect(screen.getByText(/Not created yet — the first run creates it/)).toBeInTheDocument();
    expect(screen.getByText("Not created yet.")).toBeInTheDocument();
    expect(screen.getByText(/No runs yet/)).toBeInTheDocument();
  });

  it("renders branch and worktree path once a run has created them", () => {
    render(
      <RunInfoSection
        branch="rimaia/task-1"
        worktreePath="/data/worktrees/task-1"
        lastRun={null}
        loading={false}
      />,
    );

    expect(screen.getByText("rimaia/task-1")).toBeInTheDocument();
    expect(screen.getByText("/data/worktrees/task-1")).toBeInTheDocument();
  });

  it("renders the last run's outcome", () => {
    render(
      <RunInfoSection
        branch={null}
        worktreePath={null}
        lastRun={run({ status: "failed" })}
        loading={false}
      />,
    );

    expect(screen.getByText("Failed")).toBeInTheDocument();
  });

  it("shows a loading state instead of the empty copy while detail is unresolved", () => {
    render(<RunInfoSection branch={null} worktreePath={null} lastRun={null} loading />);

    expect(screen.getByText("Loading…")).toBeInTheDocument();
    expect(screen.queryByText(/No runs yet/)).not.toBeInTheDocument();
  });
});
