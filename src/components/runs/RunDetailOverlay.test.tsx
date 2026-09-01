import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { RunDetailOverlay } from "./RunDetailOverlay";
import type { RunDetail } from "../../types";

// Mocked at the Tauri seam — see `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function runDetail(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: "run-1",
    taskId: "task-1",
    attempt: 2,
    status: "succeeded",
    sessionId: "session-1",
    prompt: "Implement the parser.",
    startedAt: "2026-08-20T11:00:00Z",
    endedAt: "2026-08-20T11:05:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 4,
    costUsd: 0.1234,
    logPath: "/data/runs/task-1/run-1.jsonl",
    prUrl: "https://github.com/abtion/rimaia/pull/42",
    resumeAfter: null,
    diff: {
      taskId: "task-1",
      branch: "rimaia/task-1-add-the-parser",
      baseRef: "main",
      diff: { filesChanged: 2, insertions: 10, deletions: 3 },
      files: [
        { path: "src/lib.rs", insertions: 8, deletions: 1 },
        { path: "logo.png", insertions: null, deletions: null },
      ],
      commits: [
        {
          sha: "1111111111111111111111111111111111111111",
          shortSha: "1111111",
          subject: "Add the parser",
          author: "Rimaia Test",
          committedAt: "2026-08-20T11:04:00Z",
        },
      ],
    },
    logAvailable: true,
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
});

describe("RunDetailOverlay", () => {
  it("renders the outcome, diff, commits, PR link and prompt in ADR-0013's order", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail();
      if (command === "read_run_transcript_page") {
        return { entries: [], offset: 0, totalLines: 0 };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("Run detail — attempt 2")).toBeInTheDocument();
    expect(screen.getByText("Succeeded")).toBeInTheDocument();
    expect(screen.getByText("$0.1234")).toBeInTheDocument();
    expect(screen.getByText(/2 files changed \(\+10 \/ -3\)/)).toBeInTheDocument();
    expect(screen.getByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByText("binary")).toBeInTheDocument();
    expect(screen.getByText(/Add the parser/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: /pull\/42/ })).toHaveAttribute(
      "href",
      "https://github.com/abtion/rimaia/pull/42",
    );
    expect(screen.getByText("Implement the parser.")).toBeInTheDocument();
    // The order itself, not only that each section is present: ADR-0013's
    // whole point is that a reviewer meets the diff and the commits before
    // the transcript. `getAllByRole` returns document order.
    expect(
      screen.getAllByRole("heading", { level: 4 }).map((heading) => heading.textContent),
    ).toEqual(["Outcome", "Diff summary", "Commits", "Pull request", "Prompt", "Transcript"]);
  });

  // Both callers mount it inside their own layout — `RunHistorySection`
  // inside the board panel's scroll container — and neither should have to be
  // the containing block of a viewport overlay. See this component's own doc.
  it("portals itself to the document body rather than rendering where it is written", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail({ logAvailable: false });
      throw new Error(`unexpected command: ${command}`);
    });

    const { container } = render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const overlay = await screen.findByRole("dialog", { name: "Run detail" });
    expect(container).not.toContainElement(overlay);
    expect(overlay.parentElement).toBe(document.body);
  });

  it("shows log unavailable instead of the transcript viewer when the file is gone", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail({ logAvailable: false });
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText(/Log unavailable/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reveal raw log" })).toBeDisabled();
  });

  it("shows a no-pull-request placeholder when none was opened", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail({ prUrl: null });
      if (command === "read_run_transcript_page") {
        return { entries: [], offset: 0, totalLines: 0 };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("No pull request opened yet.")).toBeInTheDocument();
  });
});
