import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { RunDetailOverlay } from "./RunDetailOverlay";
import { twoLoopHistory } from "../../test/reviewFixtures";
import type { ReviewHistory, RunDetail, StoredBundle } from "../../types";

// Mocked at the Tauri seam — see `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

const PATCH = "diff --git a/src/lib.rs b/src/lib.rs\n+pub fn parse() {}\n";

function bundle(overrides: Partial<StoredBundle> = {}): StoredBundle {
  return {
    diff: { filesChanged: 2, insertions: 10, deletions: 3 },
    files: [
      { path: "src/lib.rs", insertions: 8, deletions: 1, patch: "included" },
      { path: "logo.png", insertions: null, deletions: null, patch: "binary" },
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
    patch: PATCH,
    patchBytes: 1200,
    patchTruncated: false,
    patchPrunedAt: null,
    createdAt: "2026-08-20T11:05:00Z",
    ...overrides,
  };
}

/** Answers `get_run` with `detail` and fails anything unexpected, recording
 *  every command so a test can assert `get_diff_summary` was never asked. */
function answering(detail: RunDetail, diffSummary?: () => Promise<unknown>) {
  mockInvoke.mockImplementation(async (command) => {
    if (command === "get_run") return detail;
    if (command === "read_run_transcript_page") {
      return { entries: [], offset: 0, totalLines: 0 };
    }
    if (command === "get_diff_summary" && diffSummary) return diffSummary();
    throw new Error(`unexpected command: ${command}`);
  });
}

function invoked(command: string): boolean {
  return mockInvoke.mock.calls.some(([name]) => name === command);
}

function runDetail(overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: "run-1",
    taskId: "task-1",
    attempt: 2,
    kind: "implementation",
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
    baseRef: null,
    model: null,
    effort: null,
    runEnvironment: null,
    inputTokens: null,
    outputTokens: null,
    cacheReadTokens: null,
    cacheCreationTokens: null,
    headSha: "2222222222222222222222222222222222222222",
    baseSha: "0000000000000000000000000000000000000000",
    review: { source: "recorded", bundle: bundle() },
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

    expect(await screen.findByText("Run detail — Implementation · #2")).toBeInTheDocument();
    expect(screen.getByText("Succeeded")).toBeInTheDocument();
    expect(screen.getByText("$0.1234")).toBeInTheDocument();
    expect(screen.getByText(/2 files changed \(\+10 \/ -3\)/)).toBeInTheDocument();
    expect(screen.getByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByText("not in patch: binary")).toBeInTheDocument();
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

  // Task 033: a recorded review is the run's own record. Asking git for the
  // branch's current state would let it change because the branch did.
  it("never asks for the live diff when the review was recorded", async () => {
    answering(runDetail());

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("src/lib.rs")).toBeInTheDocument();
    expect(invoked("get_diff_summary")).toBe(false);
  });

  it("keeps the patch collapsed until it is asked for", async () => {
    answering(runDetail());

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const summary = await screen.findByText("Patch");
    const details = summary.closest("details");
    expect(details).not.toBeNull();
    expect(details?.open).toBe(false);
    expect(details?.querySelector("pre")?.textContent).toBe(PATCH);
  });

  it("says why each file the patch left out is missing", async () => {
    answering(
      runDetail({
        review: {
          source: "recorded",
          bundle: bundle({
            diff: { filesChanged: 4, insertions: 9, deletions: 1 },
            files: [
              { path: "package-lock.json", insertions: 9000, deletions: 0, patch: "too_large" },
              { path: "logo.png", insertions: null, deletions: null, patch: "binary" },
              { path: "latin1.txt", insertions: 2, deletions: 0, patch: "not_utf8" },
              { path: "src/lib.rs", insertions: 8, deletions: 1, patch: "included" },
            ],
            patchTruncated: true,
          }),
        },
      }),
    );

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const row = async (path: string) => (await screen.findByText(path)).closest("li");
    expect(await row("package-lock.json")).toHaveTextContent("not in patch: too large");
    expect(await row("logo.png")).toHaveTextContent("not in patch: binary");
    expect(await row("latin1.txt")).toHaveTextContent("not in patch: not UTF-8 text");
    expect(await row("src/lib.rs")).not.toHaveTextContent("not in patch");
  });

  it("points a truncated patch at the pull request for the rest", async () => {
    answering(
      runDetail({
        review: {
          source: "recorded",
          bundle: bundle({
            files: [
              { path: "a.txt", insertions: 9000, deletions: 0, patch: "too_large" },
              { path: "src/lib.rs", insertions: 8, deletions: 1, patch: "included" },
            ],
            patchBytes: 2_400_000,
            patchTruncated: true,
          }),
        },
      }),
    );

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const note = await screen.findByText(/The patch holds 1 of 2 files/);
    expect(note).toHaveTextContent("the whole diff was 2.3 MB");
    expect(within(note).getByRole("link", { name: "pull request" })).toHaveAttribute(
      "href",
      "https://github.com/abtion/rimaia/pull/42",
    );
  });

  it("says a truncated patch's rest was not stored when there is no pull request", async () => {
    answering(
      runDetail({
        prUrl: null,
        review: {
          source: "recorded",
          bundle: bundle({
            files: [
              { path: "a.txt", insertions: 9000, deletions: 0, patch: "too_large" },
              { path: "src/lib.rs", insertions: 8, deletions: 1, patch: "included" },
            ],
            patchTruncated: true,
          }),
        },
      }),
    );

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const note = await screen.findByText(/The patch holds 1 of 2 files/);
    expect(note).toHaveTextContent("The rest was not stored.");
    expect(within(note).queryByRole("link")).not.toBeInTheDocument();
  });

  it("keeps the file list and commits of a pruned patch and says when it went", async () => {
    answering(
      runDetail({
        review: {
          source: "recorded",
          bundle: bundle({ patch: null, patchPrunedAt: "2026-09-20T12:00:00Z" }),
        },
      }),
    );

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText(/The patch was pruned on/)).toBeInTheDocument();
    expect(screen.getByText("src/lib.rs")).toBeInTheDocument();
    expect(screen.getByText(/Add the parser/)).toBeInTheDocument();
    expect(screen.queryByText("Patch")).not.toBeInTheDocument();
  });

  it("says a recorded run with no commits on its branch left nothing to review", async () => {
    answering(runDetail({ review: { source: "recorded", bundle: null } }));

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(
      await screen.findByText("This run ended with no commits on its branch."),
    ).toBeInTheDocument();
    expect(invoked("get_diff_summary")).toBe(false);
  });

  it("labels the live diff of a run that recorded nothing as the branch's current state", async () => {
    answering(runDetail({ review: { source: "not_recorded" }, headSha: null }), async () => ({
      taskId: "task-1",
      branch: "rimaia/task-1-add-the-parser",
      baseRef: "main",
      diff: { filesChanged: 1, insertions: 5, deletions: 0 },
      files: [{ path: "src/live.rs", insertions: 5, deletions: 0 }],
      commits: [
        {
          sha: "3333333333333333333333333333333333333333",
          shortSha: "3333333",
          subject: "Commit made since",
          author: "Rimaia Test",
          committedAt: "2026-08-21T09:00:00Z",
        },
      ],
    }));

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(
      await screen.findByText(
        "No diff was recorded for this run. This is the branch’s current state, not what this run left.",
      ),
    ).toBeInTheDocument();
    expect(await screen.findByText("src/live.rs")).toBeInTheDocument();
    expect(screen.getByText(/Commit made since/)).toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledWith("get_diff_summary", { taskId: "task-1" });
  });

  it("renders one line for the diff when the branch of an unrecorded run cannot be read", async () => {
    answering(runDetail({ review: { source: "not_recorded" }, headSha: null }), async () => {
      throw { code: "invalid", message: "the repository has been moved or deleted" };
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    const line = await screen.findByText(
      "No diff was recorded for this run, and its branch can no longer be read.",
    );
    const section = line.closest("section");
    expect(section?.querySelectorAll("p")).toHaveLength(1);
    expect(section?.querySelector("ul")).toBeNull();
    // The rest of the overlay is unaffected, and the failure is no banner.
    expect(screen.getByText("Succeeded")).toBeInTheDocument();
    expect(screen.getByText("Implement the parser.")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Transcript" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
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

  // The run this was written for: an hour of refused commands, then a stream
  // that stopped mid-message. The row's own message ("the stream ended")
  // describes that ending without explaining it; these three lines are the
  // explanation, and every one of them was already in the transcript.
  it("says what the transcript knows about how the run ended", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") {
        return runDetail({
          exitClass: "transient",
          errorMessage: "the event stream ended without a result event",
          logAvailable: false,
        });
      }
      if (command === "summarize_run_transcript") {
        return {
          permissionMode: "acceptEdits",
          model: "claude-sonnet-5",
          deniedToolCalls: 24,
          endedWithResult: false,
          endsMidLine: true,
          malformedLines: 1,
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("acceptEdits")).toBeInTheDocument();
    expect(screen.getByText(/24 tool calls were refused for want of approval/)).toBeInTheDocument();
    expect(screen.getByText(/transcript ends mid-line/)).toBeInTheDocument();
  });

  it("says nothing about refusals or the stream when the run ended cleanly", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail({ logAvailable: false });
      if (command === "summarize_run_transcript") {
        return {
          permissionMode: "bypassPermissions",
          model: "claude-sonnet-5",
          deniedToolCalls: 0,
          endedWithResult: true,
          endsMidLine: false,
          malformedLines: 0,
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("bypassPermissions")).toBeInTheDocument();
    expect(screen.queryByText(/refused for want of approval/)).not.toBeInTheDocument();
    expect(screen.queryByText(/without a result event/)).not.toBeInTheDocument();
  });

  // The summary explains the run; it is not the run. A pruned transcript must
  // not put an error over an outcome that reads perfectly well without it.
  it("still renders the outcome when the transcript summary cannot be read", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return runDetail({ logAvailable: false });
      if (command === "summarize_run_transcript") {
        throw { code: "not_found", message: "could not open the transcript" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    expect(await screen.findByText("Succeeded")).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    // Still six sections in ADR-0013's order: the overlay's grid places them
    // by position, and a missing one would move the prompt into the rail.
    expect(
      screen.getAllByRole("heading", { level: 4 }).map((heading) => heading.textContent),
    ).toEqual(["Outcome", "Diff summary", "Commits", "Pull request", "Prompt", "Transcript"]);
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

  // -------------------------------------------------------------------------
  // The review loop's findings beside the diff (task 037)
  // -------------------------------------------------------------------------

  function answeringWithHistory(detail: RunDetail, history: ReviewHistory) {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_run") return detail;
      if (command === "get_review_history") return history;
      if (command === "read_run_transcript_page") {
        return { entries: [], offset: 0, totalLines: 0 };
      }
      throw new Error(`unexpected command: ${command}`);
    });
  }

  function titlesOfFindings(): string[] {
    return Array.from(document.querySelectorAll(".finding-title")).map(
      (element) => element.textContent ?? "",
    );
  }

  it("lists the current loop's open findings for the task's newest row, blocking before advisory", async () => {
    const history = twoLoopHistory();
    const [cameBack, brandNew, nit] = history.loops[1].rounds[1].findings;
    // Core hands them in the reviewer's order; the advisory one came first.
    history.loops[1].rounds[1].findings = [nit, cameBack, brandNew];
    answeringWithHistory(runDetail({ id: "run-7", attempt: 7, kind: "review" }), history);

    render(<RunDetailOverlay runId="run-7" onClose={() => {}} />);

    expect(await screen.findByRole("heading", { name: "Unresolved findings" })).toBeInTheDocument();
    expect(titlesOfFindings()).toEqual(["Unchecked index", "Race on logout", "Rename the helper"]);
    expect(screen.getByText("Advisory")).toBeInTheDocument();
  });

  it("puts the loop's verdict in the outcome, in the card's words", async () => {
    answeringWithHistory(runDetail({ id: "run-7", attempt: 7, kind: "review" }), twoLoopHistory());

    render(<RunDetailOverlay runId="run-7" onClose={() => {}} />);

    expect(
      await screen.findByText("Reviewed after 1 fix · 2 blocking findings open"),
    ).toBeInTheDocument();
    expect(screen.getByText("May be going in circles")).toBeInTheDocument();
    expect(screen.getByText("Run detail — Review · #7")).toBeInTheDocument();
  });

  it("lists what an older review raised, with each finding's status today", async () => {
    answeringWithHistory(runDetail({ id: "run-5", attempt: 5, kind: "review" }), twoLoopHistory());

    render(<RunDetailOverlay runId="run-5" onClose={() => {}} />);

    expect(
      await screen.findByRole("heading", { name: "Findings from this review" }),
    ).toBeInTheDocument();
    expect(titlesOfFindings()).toEqual([
      "Unchecked index",
      "Leaked file handle",
      "Wrong default timeout",
    ]);
    expect(screen.getAllByText("Fixed in #6")).toHaveLength(2);
    expect(
      screen.getByText("Rejected in #6 — The caller always passes a timeout."),
    ).toBeInTheDocument();
    // The verdict is about the loop as it stands, which an older row did not
    // leave.
    expect(screen.queryByText(/blocking findings open/)).toBeNull();
  });

  it("shows no findings for an older implementation row", async () => {
    answeringWithHistory(runDetail({ id: "run-4", attempt: 4 }), twoLoopHistory());

    render(<RunDetailOverlay runId="run-4" onClose={() => {}} />);

    await screen.findByText("Run detail — Implementation · #4");
    expect(screen.queryByRole("heading", { name: /findings/i })).toBeNull();
  });

  it("keeps the outcome with its verdict, diff, commits, PR link, findings, prompt, transcript in order", async () => {
    answeringWithHistory(runDetail({ id: "run-7", attempt: 7, kind: "review" }), twoLoopHistory());

    render(<RunDetailOverlay runId="run-7" onClose={() => {}} />);

    const verdict = await screen.findByText("Reviewed after 1 fix · 2 blocking findings open");
    const diff = screen.getByRole("heading", { name: "Diff summary" });
    const commits = screen.getByRole("heading", { name: "Commits" });
    const pullRequest = screen.getByRole("heading", { name: "Pull request" });
    const findings = screen.getByRole("heading", { name: "Unresolved findings" });
    const prompt = screen.getByRole("heading", { name: "Prompt" });
    const transcript = screen.getByRole("heading", { name: "Transcript" });

    const inOrder = [verdict, diff, commits, pullRequest, findings, prompt, transcript];
    for (let index = 0; index < inOrder.length - 1; index += 1) {
      expect(
        inOrder[index].compareDocumentPosition(inOrder[index + 1]) &
          Node.DOCUMENT_POSITION_FOLLOWING,
        `${inOrder[index].textContent} precedes ${inOrder[index + 1].textContent}`,
      ).toBeTruthy();
    }
  });

  it("is unchanged for a task the loop never touched", async () => {
    answeringWithHistory(runDetail(), { loops: [] });

    render(<RunDetailOverlay runId="run-1" onClose={() => {}} />);

    await screen.findByText("Run detail — Implementation · #2");
    expect(screen.queryByText("Review loop")).toBeNull();
    expect(screen.queryByRole("heading", { name: /findings/i })).toBeNull();
  });
});
