import type {
  DigestEntry,
  HistoryFinding,
  PhaseSummary,
  ReviewHistory,
  Repository,
  ReviewDigest,
  RunDetail,
  StoredBundle,
  TaskDependent,
  TaskDetail,
  TaskSummary,
} from "../types";

/** Factories for the morning review's tests. Plain data, no mocks. */

export function taskSummary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    repositoryId: "repo-a",
    title: "Add login",
    plan: "a plan",
    extraInstructions: null,
    column: "in_review",
    position: 1,
    runState: "idle",
    branch: "rimaia/task-1",
    worktreePath: "/data/worktrees/a/task-1",
    strategyMode: "default",
    model: null,
    effort: null,
    strategyPlan: null,
    strategySource: null,
    strategyUpdatedAt: null,
    createdAt: "2026-10-04T08:00:00Z",
    updatedAt: "2026-10-04T08:30:00Z",
    source: "ui",
    archivedAt: null,
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    blockingTitle: null,
    lastRun: { kind: "implementation", status: "succeeded", exitClass: "success", endedAt: "2026-10-04T09:00:00Z", resumeAfter: null },
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
    reviewLoop: null,
    ...overrides,
  };
}

export function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-a",
    name: "rimaia-app",
    path: "/code/rimaia-app",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees/a",
    allowUnattendedRuns: true,
    maxConcurrency: 1,
    createdAt: "2026-08-20T09:00:00Z",
    onArchive: "none",
    onArchiveScript: null,
    ...overrides,
  };
}

export const PATCH = "diff --git a/src/login.ts b/src/login.ts\n+export const login = true;\n";

export function bundle(overrides: Partial<StoredBundle> = {}): StoredBundle {
  return {
    diff: { filesChanged: 2, insertions: 10, deletions: 3 },
    files: [
      { path: "src/login.ts", insertions: 8, deletions: 1, patch: "included" },
      { path: "logo.png", insertions: null, deletions: null, patch: "binary" },
    ],
    commits: [
      {
        sha: "1111111111111111111111111111111111111111",
        shortSha: "1111111",
        subject: "Add the login form",
        author: "Rimaia",
        committedAt: "2026-10-04T08:50:00Z",
      },
    ],
    patch: PATCH,
    patchBytes: 1200,
    patchTruncated: false,
    patchPrunedAt: null,
    createdAt: "2026-10-04T09:00:00Z",
    ...overrides,
  };
}

/** A `get_task` answer whose newest run is `run-for-<id>`. */
export function taskDetail(id: string, overrides: Partial<TaskDetail> = {}): TaskDetail {
  const { lastRun: _summary, ...summary } = taskSummary({ id });
  void _summary;
  return {
    ...summary,
    links: [],
    dependsOn: [],
    lastRun: plainRun(id),
    reviewInstructions: null,
    reviewConfig: {},
    reviewLoop: null,
    ...overrides,
  };
}

function plainRun(taskId: string): NonNullable<TaskDetail["lastRun"]> {
  const { review, logAvailable, ...run } = runDetail(taskId);
  void review;
  void logAvailable;
  return run;
}

export function runDetail(taskId: string, overrides: Partial<RunDetail> = {}): RunDetail {
  return {
    id: `run-for-${taskId}`,
    taskId,
    attempt: 1,
    kind: "implementation",
    status: "succeeded",
    sessionId: "session-1",
    prompt: "Implement it.",
    startedAt: "2026-10-04T08:20:00Z",
    endedAt: "2026-10-04T09:00:00Z",
    exitClass: "success",
    errorMessage: null,
    numTurns: 12,
    costUsd: 1.5,
    logPath: `/data/runs/${taskId}.jsonl`,
    prUrl: `https://github.com/example/app/pull/${taskId}`,
    resumeAfter: null,
    baseRef: "main",
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

export function dependent(overrides: Partial<TaskDependent> = {}): TaskDependent {
  return {
    id: "dep-1",
    title: "Dependent",
    column: "ready",
    runState: "blocked",
    archivedAt: null,
    builtOn: false,
    ...overrides,
  };
}

function entry(overrides: Partial<DigestEntry>): DigestEntry {
  return {
    taskId: "t",
    title: "A task",
    repositoryId: "repo-a",
    column: "ready",
    outcome: "completed",
    runs: 0,
    runSeconds: null,
    costUsd: null,
    lastRunId: null,
    errorMessage: null,
    prUrl: null,
    blockingTitle: null,
    lastRunKind: null,
    reviewLoop: null,
    skipReason: null,
    ...overrides,
  };
}

const NO_COUNTS = {
  failed: 0,
  blocked: 0,
  waiting_retry: 0,
  interrupted: 0,
  cancelled: 0,
  running: 0,
  completed: 0,
  skipped: 0,
};

/** Task 034's six-task night: a succeeded task now in review, a failed one, a
 *  blocked dependent of the failed one, one skipped because its repository does
 *  not allow unattended runs, a cancelled one, and a succeeded one whose cost
 *  was not recorded. Its totals are deliberately not the sum of its entries:
 *  the failed task ran twice, and totals count runs that ended in the window. */
export function sixTaskDigest(overrides: Partial<ReviewDigest> = {}): ReviewDigest {
  return {
    since: "2026-10-04T01:00:00Z",
    until: "2026-10-04T09:00:00Z",
    entries: [
      entry({
        taskId: "failed",
        title: "Migrate the period selector",
        outcome: "failed",
        runs: 2,
        runSeconds: 2460,
        costUsd: 3.18,
        errorMessage: "The agent stopped after repeated tool failures.",
      }),
      entry({
        taskId: "blocked",
        title: "Wire the status colours",
        outcome: "blocked",
        blockingTitle: "Migrate the period selector",
      }),
      entry({
        taskId: "cancelled",
        title: "Prototype drag handles",
        outcome: "cancelled",
        runs: 1,
        runSeconds: 540,
        costUsd: 0.46,
      }),
      entry({
        taskId: "done",
        title: "Add login",
        column: "in_review",
        outcome: "completed",
        runs: 1,
        runSeconds: 1980,
        costUsd: 2.48,
      }),
      entry({
        taskId: "nocost",
        title: "Show the queue plan",
        column: "in_review",
        outcome: "completed",
        runs: 1,
        runSeconds: 1560,
        costUsd: null,
      }),
      entry({
        taskId: "skipped",
        title: "Refresh the footer",
        outcome: "skipped",
        skipReason: "unattended_runs_not_allowed",
      }),
    ],
    totals: {
      runs: 6,
      runSeconds: 8400,
      spanSeconds: 27000,
      costUsd: 4.2,
      runsWithoutCost: 1,
      counts: { ...NO_COUNTS, failed: 1, blocked: 1, cancelled: 1, completed: 2, skipped: 1 },
    },
    ...overrides,
  };
}

export function emptyDigest(): ReviewDigest {
  return {
    since: "2026-10-03T09:00:00Z",
    until: "2026-10-04T09:00:00Z",
    entries: [],
    totals: {
      runs: 0,
      runSeconds: 0,
      spanSeconds: null,
      costUsd: 0,
      runsWithoutCost: 0,
      counts: { ...NO_COUNTS },
    },
  };
}

// ---------------------------------------------------------------------------
// Review history (task 037)
// ---------------------------------------------------------------------------

export function phase(
  kind: PhaseSummary["kind"],
  attempts: number[],
  overrides: Partial<PhaseSummary> = {},
): PhaseSummary {
  return {
    kind,
    runIds: attempts.map((attempt) => `run-${attempt}`),
    attempts,
    status: "succeeded",
    exitClass: "success",
    ...overrides,
  };
}

export function historyFinding(
  id: string,
  reviewAttempt: number,
  overrides: Partial<HistoryFinding> = {},
): HistoryFinding {
  return {
    id,
    taskId: "task-1",
    reviewRunId: `run-${reviewAttempt}`,
    ordinal: 0,
    severity: "high",
    title: `Finding ${id}`,
    body: `Why ${id} matters.`,
    file: "src/login.ts",
    line: 12,
    fingerprint: `src/login.ts|finding ${id}`,
    status: "open",
    resolution: null,
    resolvedByRunId: null,
    createdAt: "2026-10-04T08:40:00Z",
    resolvedAt: null,
    blocking: true,
    carriedOver: false,
    ...overrides,
  };
}

/**
 * Task 037's two-loop acceptance fixture. `attempt` is one ascending sequence
 * per task (seam-contract D29 point 2), so the earlier loop is implementation
 * #1, review #2 and fix #3; the newest is implementation #4, review #5 (three
 * blocking findings), fix #6 (two fixed, one rejected with a reason), and
 * review #7, which raises one finding that came back after #6 fixed it, one
 * blocking finding new after the fix, one advisory finding, and one that task
 * 021 carried over as already rejected.
 */
export function twoLoopHistory(): ReviewHistory {
  const fixed = (id: string, ordinal: number, title: string) =>
    historyFinding(id, 5, {
      ordinal,
      title,
      status: "fixed",
      resolution: "Done.",
      resolvedByRunId: "run-6",
    });
  const first = fixed("f-index", 0, "Unchecked index");
  const second = fixed("f-handle", 1, "Leaked file handle");
  const declined = historyFinding("f-default", 5, {
    ordinal: 2,
    title: "Wrong default timeout",
    status: "rejected",
    resolution: "The caller always passes a timeout.",
    resolvedByRunId: "run-6",
  });
  const cameBack = historyFinding("f-index-again", 7, {
    ordinal: 0,
    title: "Unchecked index",
  });
  const brandNew = historyFinding("f-race", 7, { ordinal: 1, title: "Race on logout" });
  const nit = historyFinding("f-nit", 7, {
    ordinal: 2,
    title: "Rename the helper",
    severity: "low",
    blocking: false,
  });
  const carried = historyFinding("f-default-again", 7, {
    ordinal: 3,
    title: "Wrong default timeout",
    status: "rejected",
    resolution: "Rejected earlier as f-default: The caller always passes a timeout.",
    carriedOver: true,
  });

  return {
    loops: [
      {
        earlier: true,
        implementation: phase("implementation", [1]),
        rounds: [
          {
            review: phase("review", [2]),
            findings: [
              historyFinding("f-old", 2, {
                title: "Missing migration",
                status: "fixed",
                resolution: "Added it.",
                resolvedByRunId: "run-3",
              }),
            ],
            fix: { phase: phase("fix", [3]), resolved: [] },
            regressed: [],
            newAfterFix: [],
            pingPong: false,
          },
        ],
        fixesSpent: 1,
        verdict: { verdict: "unreviewed", reason: "fix_not_reviewed" },
        openBlocking: 0,
        openAdvisory: 0,
      },
      {
        earlier: false,
        implementation: phase("implementation", [4]),
        rounds: [
          {
            review: phase("review", [5]),
            findings: [first, second, declined],
            fix: { phase: phase("fix", [6]), resolved: [first, second, declined] },
            regressed: [],
            newAfterFix: [],
            pingPong: false,
          },
          {
            review: phase("review", [7]),
            findings: [cameBack, brandNew, nit, carried],
            fix: null,
            regressed: [cameBack],
            newAfterFix: [brandNew],
            pingPong: true,
          },
        ],
        fixesSpent: 1,
        verdict: { verdict: "findings_remain", openBlocking: 2 },
        openBlocking: 2,
        openAdvisory: 1,
      },
    ],
  };
}
