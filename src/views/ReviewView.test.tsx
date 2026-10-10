import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { installBackend, sent, writesSent } from "../test/reviewBackend";
import type { ReviewBackend } from "../test/reviewBackend";
import {
  bundle,
  dependent,
  emptyDigest,
  runDetail,
  sixTaskDigest,
  taskSummary,
  twoLoopHistory,
} from "../test/reviewFixtures";
import type { RunReview } from "../types";
import { ReviewView } from "./ReviewView";

// Mocked at the Tauri seam, not at `lib/commands.ts`/`lib/events.ts` — see
// `StorageSection.test.tsx`'s own comment for why.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const NOW = () => new Date("2026-10-04T09:00:00Z");

function three() {
  return [
    taskSummary({ id: "task-a", title: "Task A", position: 1 }),
    taskSummary({ id: "task-b", title: "Task B", position: 2 }),
    taskSummary({ id: "task-c", title: "Task C", position: 3 }),
  ];
}

async function openQueue(user: ReturnType<typeof userEvent.setup>, title = "Task A") {
  await screen.findByRole("region", { name: "Overnight digest" });
  await user.keyboard("{Enter}");
  return screen.findByRole("article", { name: title });
}

let backend: ReviewBackend;
let user: ReturnType<typeof userEvent.setup>;

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(listen).mockReset();
  user = userEvent.setup();
  backend = installBackend(three());
});

describe("the overnight digest", () => {
  beforeEach(() => {
    backend.digest = sixTaskDigest();
  });

  it("renders one entry per task with its word, in the order received", async () => {
    render(<ReviewView now={NOW} />);

    await screen.findByText("Runs");
    const rows = screen
      .getAllByRole("listitem")
      .filter((item) => item.classList.contains("review-entry"));

    expect(rows.map((row) => row.querySelector(".review-entry-title")?.textContent)).toEqual([
      "Migrate the period selector",
      "Wire the status colours",
      "Prototype drag handles",
      "Add login",
      "Show the queue plan",
      "Refresh the footer",
    ]);
    expect(rows.map((row) => row.querySelector(".review-entry-outcome")?.textContent)).toEqual([
      "Failed",
      "Blocked",
      "Cancelled",
      "Completed",
      "Completed",
      "Skipped",
    ]);
    expect(rows[1]).toHaveTextContent("waiting on Migrate the period selector");
    expect(rows[5]).toHaveTextContent("this repository has not enabled unattended agent runs");
    expect(rows[4]).toHaveTextContent("not recorded");
    expect(document.body.textContent).not.toContain("$0.00");
  });

  it("shows the service's totals and not a sum over the entries", async () => {
    render(<ReviewView now={NOW} />);

    const totals = (await screen.findByText("Runs")).closest("dl") as HTMLElement;
    // Entries' costs add to 6.12 and their run times to 6540s; the service says
    // 4.20 and 8400s because it counts runs that ended in the window.
    expect(within(totals).getByText("6")).toBeInTheDocument();
    expect(within(totals).getByText("2h 20m")).toBeInTheDocument();
    expect(within(totals).getByText("$4.20")).toBeInTheDocument();
    expect(within(totals).getByText("7h 30m")).toBeInTheDocument();
    expect(screen.getByText("1 run had no recorded cost.")).toBeInTheDocument();
  });

  it("leaves the no-cost line out when every run recorded one", async () => {
    const digest = sixTaskDigest();
    digest.totals.runsWithoutCost = 0;
    backend.digest = digest;
    render(<ReviewView now={NOW} />);

    await screen.findByText("Runs");
    expect(screen.queryByText(/had no recorded cost/)).not.toBeInTheDocument();
  });

  it("leaves the wall-clock span out when the service kept none", async () => {
    const digest = sixTaskDigest();
    digest.totals.spanSeconds = null;
    backend.digest = digest;
    render(<ReviewView now={NOW} />);

    await screen.findByText("Runs");
    expect(screen.queryByText("Wall-clock span")).not.toBeInTheDocument();
  });

  it("says in one line that nothing has ended when the digest is empty", async () => {
    backend.digest = emptyDigest();
    render(<ReviewView now={NOW} />);

    expect(
      await screen.findByText("Nothing has ended since the last finished review."),
    ).toBeInTheDocument();
  });

  it.each(["runs:changed", "tasks:changed", "settings:changed"])(
    "re-reads the digest on %s",
    async (event) => {
      render(<ReviewView now={NOW} />);
      await screen.findByText("Runs");
      const before = sent(backend, "get_review_digest").length;

      await act(async () => backend.fire(event, event === "settings:changed" ? null : []));

      await waitFor(() =>
        expect(sent(backend, "get_review_digest").length).toBeGreaterThan(before),
      );
    },
  );

  it("copies the summary as text", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    render(<ReviewView now={NOW} />);

    await user.click(await screen.findByRole("button", { name: "Copy summary" }));

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toMatch(/^Rimaia overnight digest, since 8h ago\n6 runs,/);
  });

  it("leaves Enter to a focused button, so the copy button still copies", async () => {
    render(<ReviewView now={NOW} />);
    const copy = await screen.findByRole("button", { name: "Copy summary" });
    // Disabled until the digest arrives, and a disabled button takes no
    // focus: Enter would then reach the panel and start the review.
    await waitFor(() => expect(copy).toBeEnabled());
    copy.focus();

    await user.keyboard("{Enter}");

    expect(screen.queryByRole("article")).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Overnight digest" })).toBeInTheDocument();
  });
});

describe("the review queue", () => {
  it("is reviewable with the keyboard alone, from the digest to the empty queue", async () => {
    render(<ReviewView now={NOW} />);

    await openQueue(user);
    // Digest and queue, back and forth.
    await user.keyboard("d");
    await screen.findByRole("region", { name: "Overnight digest" });
    await user.keyboard("{Enter}");
    await screen.findByRole("article", { name: "Task A" });

    // Next and previous, with the arrows as aliases, stopping at both ends.
    await user.keyboard("k");
    expect(screen.getByRole("article", { name: "Task A" })).toBeInTheDocument();
    await user.keyboard("j");
    expect(await screen.findByRole("article", { name: "Task B" })).toBeInTheDocument();
    await user.keyboard("{ArrowRight}");
    expect(await screen.findByRole("article", { name: "Task C" })).toBeInTheDocument();
    await user.keyboard("j");
    expect(screen.getByRole("article", { name: "Task C" })).toBeInTheDocument();
    await user.keyboard("{ArrowLeft}k");
    expect(await screen.findByRole("article", { name: "Task A" })).toBeInTheDocument();

    // Approve A.
    await user.keyboard("a");
    expect(await screen.findByRole("article", { name: "Task B" })).toBeInTheDocument();
    await act(async () => backend.fire("tasks:changed", ["task-a"]));

    // Needs changes on B, with a two-line note.
    await user.keyboard("c");
    await user.keyboard("Handle the empty state{Enter}Then the error state");
    await user.keyboard("{Control>}{Enter}{/Control}");
    expect(await screen.findByRole("article", { name: "Task C" })).toBeInTheDocument();
    await act(async () => backend.fire("tasks:changed", ["task-b"]));

    // Reject C.
    await user.keyboard("r");
    await user.keyboard("Wrong approach");
    await user.keyboard("{Meta>}{Enter}{/Meta}");
    expect(await screen.findByText("Nothing is waiting for review.")).toBeInTheDocument();
    await act(async () => backend.fire("tasks:changed", ["task-c"]));

    expect(writesSent(backend)).toEqual([
      ["approve_task", { taskId: "task-a" }],
      [
        "request_task_changes",
        { taskId: "task-b", note: "Handle the empty state\nThen the error state" },
      ],
      ["reject_task", { taskId: "task-c", note: "Wrong approach" }],
    ]);
    // Nothing the view did is announced as another door's work.
    expect(screen.queryByText(/from outside this view/)).not.toBeInTheDocument();
    expect(screen.queryByText(/is no longer on the board/)).not.toBeInTheDocument();
    // And no live git, ever: those commands are local, and the browser has none.
    expect(sent(backend, "get_diff_summary")).toEqual([]);
    expect(sent(backend, "get_worktree_status")).toEqual([]);
  });

  it("walks tasks in board order across repositories, not by position alone", async () => {
    backend.tasks = [
      taskSummary({ id: "a1", title: "A one", repositoryId: "repo-a", position: 1 }),
      taskSummary({ id: "b1", title: "B one", repositoryId: "repo-b", position: 2 }),
      taskSummary({ id: "a2", title: "A two", repositoryId: "repo-a", position: 3 }),
    ];
    render(<ReviewView now={NOW} />);

    await openQueue(user, "A one");
    await user.keyboard("j");
    expect(await screen.findByRole("article", { name: "A two" })).toBeInTheDocument();
    await user.keyboard("j");
    expect(await screen.findByRole("article", { name: "B one" })).toBeInTheDocument();
  });

  it("approves the task on screen and advances to the next in board order", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("a");

    expect(await screen.findByRole("article", { name: "Task B" })).toBeInTheDocument();
    expect(sent(backend, "approve_task")).toEqual([{ taskId: "task-a" }]);
  });

  it("says before submission what reject and needs changes each do, and sends different commands", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("r");
    const rejectStep = await screen.findByRole("form", { name: /Reject/ });
    expect(rejectStep).toHaveTextContent("its worktree is removed");
    expect(rejectStep).toHaveTextContent("kept on a set-aside branch");
    await user.keyboard("Start over{Control>}{Enter}{/Control}");
    await waitFor(() => expect(sent(backend, "reject_task")).toHaveLength(1));

    await user.keyboard("c");
    const changesStep = await screen.findByRole("form", { name: /Needs changes/ });
    expect(changesStep).toHaveTextContent("keeps its worktree and branch");
    await user.keyboard("Almost{Control>}{Enter}{/Control}");
    await waitFor(() => expect(sent(backend, "request_task_changes")).toHaveLength(1));
    expect(sent(backend, "request_task_changes")).toEqual([{ taskId: "task-b", note: "Almost" }]);
  });

  it("closes the note on Escape without invoking anything, and types letters into it", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("r");
    const field = await screen.findByRole("textbox", { name: "Note for the next run" });
    await user.keyboard("arjo");
    expect(field).toHaveValue("arjo");
    expect(writesSent(backend)).toEqual([]);
    expect(sent(backend, "plugin:opener|open_url")).toEqual([]);

    await user.keyboard("{Escape}");

    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
    expect(writesSent(backend)).toEqual([]);
    expect(screen.getByRole("article", { name: "Task A" })).toBeInTheDocument();
  });

  it("names the branch a reject set the work aside on", async () => {
    backend.setAsideBranch = "rimaia/x-2";
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("r");
    await user.keyboard("No{Control>}{Enter}{/Control}");

    const line = await screen.findByRole("status");
    expect(within(line).getByText("rimaia/x-2")).toBeInTheDocument();
    expect(line).toHaveTextContent("a pull request opened from it stays open");
  });

  it("says a rejected task had no branch to set aside", async () => {
    backend.setAsideBranch = null;
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("r");
    await user.keyboard("No{Control>}{Enter}{/Control}");

    expect(await screen.findByRole("status")).toHaveTextContent(
      "It had no branch to set aside.",
    );
  });

  it("shows a service refusal, keeps the task on screen and keeps the note", async () => {
    backend.refusals.reject_task = { code: "invalid", message: "a review note cannot be blank" };
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("r");
    await user.keyboard("   {Control>}{Enter}{/Control}");

    expect(await screen.findByText("a review note cannot be blank")).toBeInTheDocument();
    expect(screen.getByRole("article", { name: "Task A" })).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Note for the next run" })).toHaveValue("   ");
  });

  it("shows a refused approve and stays on the task", async () => {
    backend.refusals.approve_task = { code: "invalid", message: "the task has a run queued" };
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await user.keyboard("a");

    expect(await screen.findByText("the task has a run queued")).toBeInTheDocument();
    expect(screen.getByRole("article", { name: "Task A" })).toBeInTheDocument();
  });
});

describe("the chain", () => {
  beforeEach(() => {
    backend.dependents["task-a"] = [
      dependent({ id: "d1", title: "Dependent built on it", builtOn: true }),
      dependent({ id: "d2", title: "Dependent plain" }),
      dependent({ id: "d3", title: "Dependent archived", archivedAt: "2026-10-01T00:00:00Z" }),
    ];
    backend.details["task-a"] = { dependsOn: ["task-b"] };
  });

  it("names the dependents with their marks, and what the task builds on", async () => {
    render(<ReviewView now={NOW} />);
    const article = await openQueue(user);

    const dependents = await within(article).findByRole("region", { name: "Depends on this" });
    expect(dependents).toHaveTextContent("Dependent built on it");
    expect(within(dependents).getByText("Already ran on this branch")).toBeInTheDocument();
    expect(dependents).toHaveTextContent("Dependent archived");
    expect(within(dependents).getByText("Archived")).toBeInTheDocument();
    const buildsOn = within(article).getByRole("region", { name: "Builds on" });
    expect(buildsOn).toHaveTextContent("Task B");
  });

  it.each([
    ["r", "Reject"],
    ["c", "Needs changes"],
  ])("warns on %s which unarchived dependents would be blocked, before anything is sent", async (key) => {
    render(<ReviewView now={NOW} />);
    const article = await openQueue(user);
    await within(article).findByRole("region", { name: "Depends on this" });

    await user.keyboard(key);

    const warning = await screen.findByRole("note", { name: "Downstream tasks affected" });
    expect(warning).toHaveTextContent("Dependent built on it");
    expect(warning).toHaveTextContent("Dependent plain");
    expect(warning).not.toHaveTextContent("Dependent archived");
    expect(writesSent(backend)).toEqual([]);
  });

  it("does not warn for a task with no dependents, nor on approve", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user, "Task A");
    await user.keyboard("j");
    await screen.findByRole("article", { name: "Task B" });

    await user.keyboard("r");
    await screen.findByRole("textbox");
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
  });

  it("never warns on approve", async () => {
    render(<ReviewView now={NOW} />);
    const article = await openQueue(user);
    await within(article).findByRole("region", { name: "Depends on this" });

    await user.keyboard("a");

    await waitFor(() => expect(sent(backend, "approve_task")).toHaveLength(1));
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
  });
});

describe("the review of one task", () => {
  it("renders outcome, diff, commits, PR and the expanded patch in that order", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    await screen.findByText(PATCH_LINE);
    expect(sent(backend, "get_run")).toEqual([{ runId: "run-for-task-a" }]);
    const headings = screen.getAllByRole("heading", { level: 4 });
    const order = ["Outcome", "Diff summary", "Commits", "Pull request", "Patch"].map((name) =>
      headings.find((heading) => heading.textContent === name),
    ) as HTMLElement[];
    for (let index = 1; index < order.length; index += 1) {
      expect(
        order[index - 1].compareDocumentPosition(order[index]) &
          Node.DOCUMENT_POSITION_FOLLOWING,
      ).toBeTruthy();
    }
    expect(screen.getByText(PATCH_LINE).closest("details")).toBeNull();
  });

  describe("the review loop (task 037)", () => {
    const remaining = {
      enabled: true,
      maxReviewLoops: 2,
      fixesSpent: 1,
      reviews: 2,
      verdict: { verdict: "findings_remain", openBlocking: 2 },
      openBlocking: 2,
      openAdvisory: 1,
      pingPong: true,
    } as const;

    it("puts the verdict in the outcome and the unresolved findings after the PR link, before the patch", async () => {
      backend.tasks[0] = taskSummary({ id: "task-a", title: "Task A", reviewLoop: remaining });
      backend.histories["task-a"] = twoLoopHistory();
      render(<ReviewView now={NOW} />);
      await openQueue(user);

      await screen.findByText(PATCH_LINE);
      const verdict = await screen.findByText("Reviewed after 1 fix · 2 blocking findings open");
      expect(screen.getByText("May be going in circles")).toBeInTheDocument();
      const heading = (name: string) => screen.getByRole("heading", { level: 4, name });
      const inOrder = [
        verdict,
        heading("Diff summary"),
        heading("Commits"),
        heading("Pull request"),
        await screen.findByRole("heading", { level: 4, name: "Unresolved findings" }),
        heading("Patch"),
      ];
      for (let index = 1; index < inOrder.length; index += 1) {
        expect(
          inOrder[index - 1].compareDocumentPosition(inOrder[index]) &
            Node.DOCUMENT_POSITION_FOLLOWING,
          `${inOrder[index - 1].textContent} precedes ${inOrder[index].textContent}`,
        ).toBeTruthy();
      }
      expect(within(verdict.closest("section") as HTMLElement).getByText("Outcome")).toBeTruthy();
    });

    it("lists the same open findings the run detail does, blocking first", async () => {
      backend.tasks[0] = taskSummary({ id: "task-a", title: "Task A", reviewLoop: remaining });
      backend.histories["task-a"] = twoLoopHistory();
      render(<ReviewView now={NOW} />);
      await openQueue(user);

      await screen.findByRole("heading", { level: 4, name: "Unresolved findings" });
      const titles = Array.from(document.querySelectorAll(".finding-title")).map(
        (element) => element.textContent,
      );
      expect(titles).toEqual(["Unchecked index", "Race on logout", "Rename the helper"]);
    });

    it("shows no loop line and no findings for a task the loop never touched", async () => {
      render(<ReviewView now={NOW} />);
      await openQueue(user);

      await screen.findByText(PATCH_LINE);
      expect(screen.queryByText(/Reviewed (once|after)/)).toBeNull();
      expect(screen.queryByRole("heading", { name: /findings/i })).toBeNull();
    });
  });

  it("shows Interrupted for an interrupted run while the task's run state is failed", async () => {
    backend.tasks[0] = taskSummary({ id: "task-a", title: "Task A", runState: "failed" });
    backend.runs["run-for-task-a"] = runDetail("task-a", {
      status: "failed",
      exitClass: "interrupted",
    });
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    const outcome = (await screen.findByRole("heading", { name: "Outcome" })).closest("section");
    expect(outcome).toHaveTextContent("Interrupted");
  });

  it.each<[string, RunReview, string, string | null]>([
    [
      "truncated",
      {
        source: "recorded",
        bundle: bundle({
          files: [
            { path: "a.lock", insertions: 9000, deletions: 0, patch: "too_large" },
            { path: "src/login.ts", insertions: 8, deletions: 1, patch: "included" },
          ],
          patchBytes: 2_400_000,
          patchTruncated: true,
        }),
      },
      "The patch holds 1 of 2 files",
      "2 files changed",
    ],
    [
      "pruned",
      { source: "recorded", bundle: bundle({ patch: null, patchPrunedAt: "2026-09-20T12:00:00Z" }) },
      "The patch was pruned on",
      "2 files changed",
    ],
    [
      "no commits",
      { source: "recorded", bundle: null },
      "This run ended with no commits on its branch.",
      null,
    ],
    [
      "not recorded",
      { source: "not_recorded" },
      "No diff was recorded for this run.",
      null,
    ],
  ])("renders the %s variant's own line", async (_name, review, line, fileCount) => {
    backend.runs["run-for-task-a"] = runDetail("task-a", { review });
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    expect(await screen.findByText(new RegExp(line))).toBeInTheDocument();
    if (fileCount) expect(screen.getByText(new RegExp(fileCount))).toBeInTheDocument();
    else expect(screen.queryByText(/files? changed/)).not.toBeInTheDocument();
  });

  it("never reads the live diff for a not-recorded run, whatever the branch holds", async () => {
    backend.runs["run-for-task-a"] = runDetail("task-a", { review: { source: "not_recorded" } });
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    expect(await screen.findByText("No diff was recorded for this run.")).toBeInTheDocument();
    expect(sent(backend, "get_diff_summary")).toEqual([]);
    expect(screen.queryByText("src/live.rs")).not.toBeInTheDocument();
  });

  it("says a task with no run has none to review", async () => {
    backend.details["task-a"] = { lastRun: null };
    render(<ReviewView now={NOW} />);
    await openQueue(user);

    expect(await screen.findByText("This task has no run to review.")).toBeInTheDocument();
    expect(sent(backend, "get_run")).toEqual([]);
  });
});

describe("open PR and open worktree", () => {
  it("opens the newest run's PR exactly", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    await user.keyboard("o");

    await waitFor(() =>
      expect(sent(backend, "plugin:opener|open_url")).toEqual([
        { url: "https://github.com/example/app/pull/task-a", with: undefined },
      ]),
    );
  });

  it("opens nothing, and says so, when the newest run has no PR even if an older one had", async () => {
    backend.details["task-a"] = {
      lastRun: { ...runDetailPlain("task-a"), prUrl: null },
    };
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    await user.keyboard("o");

    expect(await screen.findByRole("status")).toHaveTextContent(
      "No pull request was recorded for the latest run.",
    );
    expect(sent(backend, "plugin:opener|open_url")).toEqual([]);
  });

  it("shows the failure when the opener refuses", async () => {
    backend.refusals["plugin:opener|open_url"] = "not allowed";
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    await user.keyboard("o");

    expect(await screen.findByText("not allowed")).toBeInTheDocument();
  });

  it("opens the Open in menu on w for a task with a worktree", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByRole("button", { name: "Open in" });

    await user.keyboard("w");

    expect(await screen.findByRole("menu", { name: "Open the worktree in" })).toBeInTheDocument();
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByRole("menu")).not.toBeInTheDocument());
  });

  it("does nothing on w for a task without a worktree", async () => {
    backend.tasks[0] = taskSummary({ id: "task-a", title: "Task A" });
    backend.withoutWorktree = ["task-a"];
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    await user.keyboard("w");

    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Open in" })).not.toBeInTheDocument();
  });
});

describe("the queue follows other doors", () => {
  it("moves to the next task and says what happened when the one on screen leaves", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    // An approve over MCP, say.
    backend.tasks[0].column = "done";
    await act(async () => backend.fire("tasks:changed", ["task-a"]));

    expect(await screen.findByRole("article", { name: "Task B" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent(
      "“Task A” was moved to Done from outside this view.",
    );
  });

  it("inserts a task that arrives in review in board order", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);

    backend.tasks.push(taskSummary({ id: "task-new", title: "Task New", position: 1.5 }));
    await act(async () => backend.fire("tasks:changed", ["task-new"]));

    const list = await screen.findByRole("list", { name: "In review" });
    await waitFor(() =>
      expect(within(list).getAllByRole("button").map((button) => button.textContent)).toEqual([
        "Task Arimaia-app",
        "Task Newrimaia-app",
        "Task Brimaia-app",
        "Task Crimaia-app",
      ]),
    );
  });

  it("re-reads the task on screen on an empty payload, which means every id", async () => {
    render(<ReviewView now={NOW} />);
    await openQueue(user);
    await screen.findByText(PATCH_LINE);
    const before = sent(backend, "get_task").length;

    await act(async () => backend.fire("tasks:changed", []));

    await waitFor(() => expect(sent(backend, "get_task").length).toBeGreaterThan(before));
  });
});

// ---------------------------------------------------------------------------

const PATCH_LINE = /diff --git a\/src\/login\.ts/;

function runDetailPlain(taskId: string) {
  const { review, logAvailable, ...run } = runDetail(taskId);
  void review;
  void logAvailable;
  return run;
}
