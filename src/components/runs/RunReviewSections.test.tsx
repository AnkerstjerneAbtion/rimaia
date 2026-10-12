import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { bundle, PATCH } from "../../test/reviewFixtures";
import type { RunReview } from "../../types";
import { RunReviewSections } from "./RunReviewSections";

// Mocked at the Tauri seam — see `StorageSection.test.tsx`'s comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

const LIVE = {
  taskId: "task-1",
  branch: "rimaia/task-1",
  baseRef: "main",
  diff: { filesChanged: 1, insertions: 5, deletions: 0 },
  files: [{ path: "src/live.rs", insertions: 5, deletions: 0 }],
  commits: [],
};

function renderSections(
  review: RunReview,
  liveDiff: "fallback" | "none",
  patch: "collapsed" | "expanded" = "collapsed",
) {
  return render(
    <RunReviewSections
      taskId="task-1"
      review={review}
      prUrl="https://github.com/example/app/pull/1"
      liveDiff={liveDiff}
      patch={patch}
    />,
  );
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === "get_diff_summary") return LIVE;
    throw new Error(`unexpected command: ${command}`);
  });
});

describe("RunReviewSections liveDiff", () => {
  it("asks get_diff_summary once for a not-recorded review when it may fall back", async () => {
    renderSections({ source: "not_recorded" }, "fallback");

    expect(await screen.findByText("src/live.rs")).toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke).toHaveBeenCalledWith("get_diff_summary", { taskId: "task-1" });
  });

  it("invokes nothing for a not-recorded review when told none, and says so", async () => {
    renderSections({ source: "not_recorded" }, "none");

    expect(screen.getByText("No diff was recorded for this run.")).toBeInTheDocument();
    await waitFor(() => expect(mockInvoke).not.toHaveBeenCalled());
    expect(screen.queryByText("src/live.rs")).not.toBeInTheDocument();
  });

  it.each(["fallback", "none"] as const)(
    "never asks for the live diff of a recorded review under %s",
    async (liveDiff) => {
      renderSections({ source: "recorded", bundle: bundle() }, liveDiff);

      expect(await screen.findByText("src/login.ts")).toBeInTheDocument();
      expect(mockInvoke).not.toHaveBeenCalled();
    },
  );
});

describe("RunReviewSections patch", () => {
  it("keeps the patch in a closed details when collapsed", () => {
    renderSections({ source: "recorded", bundle: bundle() }, "none", "collapsed");

    const details = screen.getByText("Patch").closest("details");
    expect(details?.open).toBe(false);
    expect(details?.querySelector("pre")?.textContent).toBe(PATCH);
  });

  it("puts the patch in a section after the pull request, outside any details, when expanded", () => {
    renderSections({ source: "recorded", bundle: bundle() }, "none", "expanded");

    const headings = screen.getAllByRole("heading", { level: 4 }).map((h) => h.textContent);
    expect(headings).toEqual(["Diff summary", "Commits", "Pull request", "Patch"]);
    expect(screen.getByText((_, node) => node?.tagName === "PRE" && node.textContent === PATCH))
      .toBeInTheDocument();
    expect(document.querySelector("details")).toBeNull();
  });

  it("says after the expanded patch how much of the diff is in it and where the rest is", () => {
    renderSections(
      {
        source: "recorded",
        bundle: bundle({
          files: [
            { path: "a.txt", insertions: 9000, deletions: 0, patch: "too_large" },
            { path: "src/login.ts", insertions: 8, deletions: 1, patch: "included" },
          ],
          patchBytes: 2_400_000,
          patchTruncated: true,
        }),
      },
      "none",
      "expanded",
    );

    const patchSection = screen.getByRole("heading", { name: "Patch" }).closest("section");
    expect(patchSection).toHaveTextContent("The patch holds 1 of 2 files");
    expect(patchSection).toHaveTextContent("the whole diff was 2.3 MB");
    expect(screen.getByText("not in patch: too large")).toBeInTheDocument();
  });

  it("says the rest is on the branch when a truncated review has no pull request", () => {
    render(
      <RunReviewSections
        taskId="task-1"
        review={{
          source: "recorded",
          bundle: bundle({ patchTruncated: true, patchBytes: 2_400_000 }),
        }}
        prUrl={null}
        liveDiff="none"
        patch="expanded"
      />,
    );

    expect(screen.getByText(/The rest was not stored\. It is on the branch\./)).toBeInTheDocument();
  });

  it("puts the pruned line where the expanded patch would be, with the file list kept", () => {
    renderSections(
      {
        source: "recorded",
        bundle: bundle({ patch: null, patchPrunedAt: "2026-09-20T12:00:00Z" }),
      },
      "none",
      "expanded",
    );

    const patchSection = screen.getByRole("heading", { name: "Patch" }).closest("section");
    expect(patchSection).toHaveTextContent("The patch was pruned on");
    expect(screen.getByText("src/login.ts")).toBeInTheDocument();
    expect(screen.getByText(/Add the login form/)).toBeInTheDocument();
  });
});
