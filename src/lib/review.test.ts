import { describe, expect, it } from "vitest";

import { sixTaskDigest, emptyDigest, taskSummary, dependent } from "../test/reviewFixtures";
import { groupIntoColumns } from "./board";
import type { ReviewLoopSummary, RunCostSummary, Verdict } from "../types";
import {
  PING_PONG_TEXT,
  affectedDependents,
  describeDeparture,
  digestAsText,
  entryFigures,
  finishedLoop,
  formatDigestCost,
  formatSeconds,
  openingView,
  reviewCommandForKey,
  reviewHistoryHeadText,
  reviewLoopCostNote,
  reviewLoopText,
  reviewQueue,
  runOutcome,
  successor,
} from "./review";

const NOW = new Date("2026-10-04T09:00:00Z");

function key(name: string, mods: Partial<KeyboardEvent> = {}) {
  return { key: name, ctrlKey: false, metaKey: false, altKey: false, ...mods };
}

describe("openingView", () => {
  it("opens on welcome while onboarding is not dismissed, whatever the digest", () => {
    expect(openingView(false, sixTaskDigest())).toBe("welcome");
  });

  it("opens on review when at least one run ended since the last finished review", () => {
    expect(openingView(true, sixTaskDigest())).toBe("review");
  });

  it("opens on the board when the digest holds only blocked and skipped entries", () => {
    const digest = sixTaskDigest({
      entries: sixTaskDigest().entries.filter(
        (entry) => entry.outcome === "blocked" || entry.outcome === "skipped",
      ),
    });
    digest.totals.runs = 0;
    expect(openingView(true, digest)).toBe("board");
  });

  it("opens on the board for an empty digest", () => {
    expect(openingView(true, emptyDigest())).toBe("board");
  });

  it("opens on the board when the digest read failed", () => {
    expect(openingView(true, null)).toBe("board");
  });
});

describe("reviewQueue", () => {
  it("is in_review in board order, one repository after the other, archived excluded", () => {
    const tasks = [
      taskSummary({ id: "a1", repositoryId: "repo-a", position: 1, createdAt: "2026-10-04T08:03:00Z" }),
      taskSummary({ id: "b1", repositoryId: "repo-b", position: 2, createdAt: "2026-10-04T08:02:00Z" }),
      taskSummary({ id: "a2", repositoryId: "repo-a", position: 3, createdAt: "2026-10-04T08:01:00Z" }),
      taskSummary({ id: "gone", repositoryId: "repo-a", position: 2, archivedAt: "2026-10-04T08:59:00Z" }),
      taskSummary({ id: "ready", repositoryId: "repo-a", position: 0, column: "ready" }),
      taskSummary({ id: "done", repositoryId: "repo-b", position: 0, column: "done" }),
    ];

    const queue = reviewQueue(tasks);

    expect(queue.map((task) => task.id)).toEqual(["a1", "a2", "b1"]);
    expect(queue.map((task) => task.id)).toEqual(
      groupIntoColumns(tasks.filter((task) => task.archivedAt === null)).in_review.map(
        (task) => task.id,
      ),
    );
  });
});

describe("successor", () => {
  const ids = ["a", "b", "c"];

  it("is the next task still in the queue", () => {
    expect(successor(ids, "a", ["b", "c"])).toBe("b");
    expect(successor(ids, "b", ["a", "c"])).toBe("c");
  });

  it("skips a following task that has also gone", () => {
    expect(successor(ids, "a", ["c"])).toBe("c");
  });

  it("falls back to the nearest one before when nothing follows", () => {
    expect(successor(ids, "c", ["a", "b"])).toBe("b");
  });

  it("is nothing when the queue is empty", () => {
    expect(successor(ids, "a", [])).toBeNull();
  });

  it("starts from the first remaining task for an id it never held", () => {
    expect(successor(ids, "z", ["b", "c"])).toBe("b");
  });
});

describe("reviewCommandForKey", () => {
  it("maps the queue keys, with arrows as aliases of j and k", () => {
    const commands = Object.fromEntries(
      ["j", "ArrowRight", "k", "ArrowLeft", "a", "r", "c", "o", "w", "d"].map((name) => [
        name,
        reviewCommandForKey("queue", key(name)),
      ]),
    );
    expect(commands).toEqual({
      j: "next",
      ArrowRight: "next",
      k: "previous",
      ArrowLeft: "previous",
      a: "approve",
      r: "reject",
      c: "request_changes",
      o: "open_pull_request",
      w: "open_worktree",
      d: "show_digest",
    });
  });

  it("binds only Enter on the digest", () => {
    expect(reviewCommandForKey("digest", key("Enter"))).toBe("start_review");
    expect(reviewCommandForKey("digest", key("a"))).toBeNull();
    expect(reviewCommandForKey("queue", key("Enter"))).toBeNull();
  });

  it("never treats a chord as a shortcut", () => {
    expect(reviewCommandForKey("queue", key("r", { metaKey: true }))).toBeNull();
    expect(reviewCommandForKey("queue", key("a", { ctrlKey: true }))).toBeNull();
    expect(reviewCommandForKey("digest", key("Enter", { metaKey: true }))).toBeNull();
  });
});

describe("runOutcome", () => {
  it("reads interrupted off the exit class, not the status", () => {
    expect(runOutcome({ status: "failed", exitClass: "interrupted" }).label).toBe("Interrupted");
  });

  it("names every other status", () => {
    expect(runOutcome({ status: "succeeded", exitClass: "success" })).toEqual({
      label: "Succeeded",
      tone: "success",
    });
    expect(runOutcome({ status: "failed", exitClass: "fatal" }).label).toBe("Failed");
    expect(runOutcome({ status: "cancelled", exitClass: "cancelled" }).tone).toBe("cancelled");
    expect(runOutcome({ status: "running", exitClass: null }).tone).toBe("running");
  });
});

describe("formatting", () => {
  it("formats seconds as seconds, minutes or hours", () => {
    expect(formatSeconds(3)).toBe("3s");
    expect(formatSeconds(754)).toBe("12m 34s");
    expect(formatSeconds(8400)).toBe("2h 20m");
  });

  it("keeps four places for a sub-cent cost and two otherwise", () => {
    expect(formatDigestCost(4.2)).toBe("$4.20");
    expect(formatDigestCost(0.0042)).toBe("$0.0042");
  });

  it("says not recorded for a run's null figures and nothing for an entry with no run", () => {
    const [failed, blocked, , , noCost] = sixTaskDigest().entries;
    expect(entryFigures(failed)).toEqual({ duration: "41m 0s", cost: "$3.18" });
    expect(entryFigures(noCost)).toEqual({ duration: "26m 0s", cost: "not recorded" });
    expect(entryFigures(blocked)).toBeNull();
  });
});

describe("digestAsText", () => {
  it("renders the six-task digest as one exact string, in the order received", () => {
    expect(digestAsText(sixTaskDigest(), NOW)).toBe(
      [
        "Rimaia overnight digest, since 8h ago",
        "6 runs, 2h 20m run time, $4.20 (1 run had no recorded cost)",
        "Wall-clock span 7h 30m",
        "1 failed, 1 blocked, 1 cancelled, 2 completed, 1 skipped",
        "",
        "- Failed: Migrate the period selector (41m 0s, $3.18)",
        "- Blocked: Wire the status colours (waiting on Migrate the period selector)",
        "- Cancelled: Prototype drag handles (9m 0s, $0.46)",
        "- Completed: Add login (33m 0s, $2.48)",
        "- Completed: Show the queue plan (26m 0s, cost not recorded)",
        "- Skipped: Refresh the footer (this repository has not enabled unattended agent runs)",
      ].join("\n"),
    );
  });

  it("says in one line that nothing has ended for an empty digest", () => {
    expect(digestAsText(emptyDigest(), NOW)).toBe(
      "Rimaia overnight digest, since 1d ago\nNothing has ended since the last finished review.",
    );
  });

  it("leaves the span out when the service kept none", () => {
    const digest = sixTaskDigest();
    digest.totals.spanSeconds = null;
    expect(digestAsText(digest, NOW)).not.toContain("Wall-clock");
  });
});

describe("dependents", () => {
  it("warns only about the unarchived ones", () => {
    const list = [
      dependent({ id: "x" }),
      dependent({ id: "y", archivedAt: "2026-10-01T00:00:00Z" }),
    ];
    expect(affectedDependents(list).map((item) => item.id)).toEqual(["x"]);
  });

  it("describes where a task went, or that it left the board", () => {
    expect(describeDeparture("Add login", { column: "done" })).toBe(
      "“Add login” was moved to Done from outside this view.",
    );
    expect(describeDeparture("Add login", undefined)).toBe(
      "“Add login” is no longer on the board.",
    );
  });
});

function loop(verdict: Verdict, fixesSpent = 0, overrides: Partial<ReviewLoopSummary> = {}) {
  return {
    enabled: true,
    maxReviewLoops: 2,
    fixesSpent,
    reviews: fixesSpent + 1,
    verdict,
    openBlocking: verdict.verdict === "findings_remain" ? verdict.openBlocking : 0,
    openAdvisory: 0,
    pingPong: false,
    ...overrides,
  } satisfies ReviewLoopSummary;
}

describe("reviewLoopText", () => {
  it("says nothing for a task the loop never touched", () => {
    expect(reviewLoopText(null)).toBeNull();
    expect(reviewLoopText(undefined)).toBeNull();
    expect(reviewLoopText(loop({ verdict: "none" }))).toBeNull();
  });

  it("counts fixes in the prefix, with the singular forms", () => {
    const cases: Array<[ReviewLoopSummary, string]> = [
      [loop({ verdict: "clean" }, 0), "Reviewed once · nothing blocking"],
      [loop({ verdict: "clean" }, 2), "Reviewed after 2 fixes · nothing blocking"],
      [loop({ verdict: "clean" }, 1), "Reviewed after 1 fix · nothing blocking"],
      [
        loop({ verdict: "findings_remain", openBlocking: 1 }, 0),
        "Reviewed once · 1 blocking finding open",
      ],
      [
        loop({ verdict: "findings_remain", openBlocking: 3 }, 2),
        "Reviewed after 2 fixes · 3 blocking findings open",
      ],
    ];
    for (const [summary, text] of cases) {
      expect(reviewLoopText(summary)).toBe(text);
    }
  });

  it("gives every reason a loop can end unreviewed its own sentence", () => {
    const reasons = {
      not_reviewed: "Not reviewed",
      review_failed: "Not reviewed — the review run failed",
      nothing_recorded: "Not reviewed — the reviewer recorded nothing",
      review_changed_branch: "Not reviewed — the reviewer changed the branch",
      fix_not_reviewed: "Not reviewed since the last fix",
    } as const;
    for (const [reason, text] of Object.entries(reasons)) {
      const verdict = { verdict: "unreviewed", reason } as Verdict;
      expect(reviewLoopText(loop(verdict))).toBe(text);
    }
  });

  it("never reads like a pass", () => {
    const everything: ReviewLoopSummary[] = [
      loop({ verdict: "clean" }, 0),
      loop({ verdict: "clean" }, 3),
      loop({ verdict: "findings_remain", openBlocking: 2 }, 1),
      loop({ verdict: "unreviewed", reason: "not_reviewed" }),
      loop({ verdict: "unreviewed", reason: "review_failed" }),
      loop({ verdict: "unreviewed", reason: "nothing_recorded" }),
      loop({ verdict: "unreviewed", reason: "review_changed_branch" }),
      loop({ verdict: "unreviewed", reason: "fix_not_reviewed" }),
    ];
    for (const summary of everything) {
      const text = reviewLoopText(summary) ?? "";
      expect(text).not.toMatch(/✓|✔|clean|passed/i);
    }
    expect(PING_PONG_TEXT).toBe("May be going in circles");
  });
});

describe("finishedLoop", () => {
  const summary = loop({ verdict: "findings_remain", openBlocking: 1 });

  it("withholds the verdict while the task is queued, running or waiting for a retry", () => {
    for (const runState of ["queued", "running", "waiting_retry"] as const) {
      expect(finishedLoop(runState, summary), runState).toBeNull();
    }
  });

  it("keeps it once the task has stopped moving", () => {
    for (const runState of ["idle", "failed", "cancelled", "blocked"] as const) {
      expect(finishedLoop(runState, summary), runState).toBe(summary);
    }
    expect(finishedLoop("idle", null)).toBeNull();
  });
});

describe("reviewHistoryHeadText", () => {
  const head = (
    fixesSpent: number,
    openBlocking: number,
    openAdvisory: number,
    verdict?: Verdict,
  ) => ({
    fixesSpent,
    openBlocking,
    openAdvisory,
    verdict:
      verdict ??
      (openBlocking > 0
        ? ({ verdict: "findings_remain", openBlocking } as const)
        : ({ verdict: "clean" } as const)),
  });

  it("states what remains in the current loop, from core's counts", () => {
    expect(reviewHistoryHeadText(head(2, 3, 1))).toBe(
      "Reviewed after 2 fixes · 3 blocking findings open, 1 advisory",
    );
    expect(reviewHistoryHeadText(head(0, 1, 0))).toBe(
      "Reviewed once · 1 blocking finding open",
    );
    expect(reviewHistoryHeadText(head(0, 0, 2))).toBe(
      "Reviewed once · nothing blocking, 2 advisory",
    );
    expect(reviewHistoryHeadText(head(1, 0, 0))).toBe(
      "Reviewed after 1 fix · nothing open",
    );
  });

  it("uses the card's text for a loop that was not reviewed", () => {
    expect(
      reviewHistoryHeadText(head(1, 0, 0, { verdict: "unreviewed", reason: "fix_not_reviewed" })),
    ).toBe("Not reviewed since the last fix");
  });

  it("has nothing to say for a loop without a verdict", () => {
    expect(reviewHistoryHeadText(head(0, 0, 0, { verdict: "none" }))).toBeNull();
  });
});

describe("reviewLoopCostNote", () => {
  const measured: RunCostSummary = {
    medianUsd: 0.84,
    sampleSize: 12,
    inheritCostUsd: null,
    providerDisplayName: "Claude Code",
  };

  it("prices up to 2N + 1 sessions at the measured median", () => {
    expect(reviewLoopCostNote(2, measured)).toBe(
      "With up to 2 fix loops, each task runs up to 5 more sessions: a review, then a fix and another review per loop. At your median run so far ($0.84 across 12 runs), that is up to about $4.20 more per task.",
    );
  });

  it("describes report-only mode as one review and nothing fixed", () => {
    expect(reviewLoopCostNote(0, measured)).toBe(
      "With no fix loops, each task runs 1 more session: a review that reports findings and fixes nothing. At your median run so far ($0.84 across 12 runs), that is about $0.84 more per task.",
    );
  });

  it("states no figure it did not measure", () => {
    const sentence =
      "With up to 2 fix loops, each task runs up to 5 more sessions: a review, then a fix and another review per loop. There is no finished run with a cost yet to put a price on that.";
    expect(reviewLoopCostNote(2, { ...measured, medianUsd: null, sampleSize: 0 })).toBe(sentence);
    expect(reviewLoopCostNote(2, null)).toBe(sentence);
  });

  it("uses the singular for one fix loop and one run", () => {
    expect(reviewLoopCostNote(1, { ...measured, medianUsd: 1, sampleSize: 1 })).toBe(
      "With up to 1 fix loop, each task runs up to 3 more sessions: a review, then a fix and another review per loop. At your median run so far ($1.00 across 1 run), that is up to about $3.00 more per task.",
    );
  });
});
