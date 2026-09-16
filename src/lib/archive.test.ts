import { describe, expect, it } from "vitest";

import {
  describeArchiveReport,
  describeCleanup,
  describeCleanupIntent,
  needsAttention,
} from "./archive";
import type { ArchiveReport, OnArchiveOutcome, Repository } from "../types";

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees/rimaia",
    allowUnattendedRuns: true,
    maxConcurrency: 1,
    createdAt: "2026-08-20T09:00:00Z",
    onArchive: "none",
    onArchiveScript: null,
    ...overrides,
  };
}

function archived(title: string, cleanup: OnArchiveOutcome) {
  return { taskId: title, title, archivedAt: "2026-09-15T10:00:00Z", cleanup };
}

describe("describeCleanup", () => {
  it("says nothing at all when the repository asked for nothing", () => {
    expect(describeCleanup({ kind: "nothing" })).toBeNull();
  });

  it("names the disk a worktree removal reclaimed", () => {
    expect(describeCleanup({ kind: "worktreeRemoved", bytesFreed: 2_400_000 })).toBe(
      "Removed its worktree, freeing 2.3 MB.",
    );
  });

  it("distinguishes a script that failed from one that never finished", () => {
    // `null` is the archive timeout's own signature, and "exited null" would
    // be a sentence about a number that does not exist.
    expect(describeCleanup({ kind: "scriptRan", exitCode: 3, output: "" })).toBe(
      "Your cleanup script exited 3.",
    );
    expect(describeCleanup({ kind: "scriptRan", exitCode: null, output: "" })).toBe(
      "Your cleanup script did not finish and was stopped.",
    );
  });

  it("repeats a refusal verbatim rather than paraphrasing it", () => {
    // The service's sentence names the count and the next step; a summary here
    // would be a second, worse copy of it.
    expect(
      describeCleanup({ kind: "failed", reason: '"Parser" has 3 uncommitted changes' }),
    ).toBe('"Parser" has 3 uncommitted changes');
  });
});

describe("describeCleanupIntent", () => {
  it("says nothing when archiving deletes nothing", () => {
    expect(describeCleanupIntent(repository())).toBeNull();
    expect(describeCleanupIntent(undefined)).toBeNull();
  });

  it("warns that the worktree goes, and that the branch does not", () => {
    const sentence = describeCleanupIntent(repository({ onArchive: "remove_worktree" }));
    expect(sentence).toContain("worktree will be deleted");
    expect(sentence).toContain("branch is kept");
  });

  it("names the script and says Rimaia guards nothing about it", () => {
    // ADR-0025 point 4 puts this obligation on the copy rather than the code:
    // a script gives up every guard task 016 built.
    const sentence = describeCleanupIntent(
      repository({ onArchive: "script", onArchiveScript: "/opt/teardown.sh" }),
    );
    expect(sentence).toContain("/opt/teardown.sh");
    expect(sentence).toContain("none of its own guards");
  });
});

describe("describeArchiveReport", () => {
  it("counts a clean bulk archive and says nothing else", () => {
    const report: ArchiveReport = {
      archived: [archived("One", { kind: "nothing" }), archived("Two", { kind: "nothing" })],
      refused: [],
    };

    expect(describeArchiveReport(report)).toBe("Archived 2 tasks.");
  });

  it("names every refusal, so a bulk guard is not silent", () => {
    const report: ArchiveReport = {
      archived: [archived("One", { kind: "nothing" })],
      refused: [{ taskId: "t2", title: "Two", reason: "it is running" }],
    };

    expect(describeArchiveReport(report)).toBe(
      "Archived 1 task. Left 1 alone — Two: it is running",
    );
  });

  it("surfaces a cleanup that failed on a task that was archived anyway", () => {
    // The archive committed and the cleanup did not — both halves are true at
    // once, and a summary that reported only the count would hide the half the
    // user has to act on.
    const report: ArchiveReport = {
      archived: [
        archived("One", { kind: "failed", reason: "3 uncommitted changes" }),
        archived("Two", { kind: "worktreeRemoved", bytesFreed: 1_000_000 }),
      ],
      refused: [],
    };

    const sentence = describeArchiveReport(report);
    expect(sentence).toContain("Archived 2 tasks.");
    expect(sentence).toContain("Cleanup did not go cleanly for 1 of them");
    expect(sentence).toContain("One: 3 uncommitted changes");
  });
});

describe("needsAttention", () => {
  it("agrees with the rule core enforces", () => {
    expect(needsAttention({ kind: "nothing" })).toBe(false);
    expect(needsAttention({ kind: "worktreeRemoved", bytesFreed: 0 })).toBe(false);
    expect(needsAttention({ kind: "scriptRan", exitCode: 0, output: "" })).toBe(false);
    expect(needsAttention({ kind: "scriptRan", exitCode: 1, output: "" })).toBe(true);
    expect(needsAttention({ kind: "scriptRan", exitCode: null, output: "" })).toBe(true);
    expect(needsAttention({ kind: "failed", reason: "no" })).toBe(true);
  });
});
