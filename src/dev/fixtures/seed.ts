import type {
  Analytics,
  AppInfo,
  BoardColumn,
  DiffSummary,
  DigestEntry,
  DoctorCheckResult,
  DoctorReport,
  QueueEntry,
  QueueStatus,
  CheckoutView,
  LocalWorktree,
  Repository,
  ReviewDigest,
  ReviewFinding,
  ReviewConfig,
  ReviewHistory,
  ReviewLevel,
  ReviewLoopSummary,
  ReviewSettings,
  RimaiaError,
  Run,
  RunCapacity,
  RunKind,
  RunListEntry,
  RunReview,
  RunStatus,
  RunTail,
  StoredBundle,
  TaskDependent,
  TaskReview,
  TaskSummary,
} from "../../types";
import { FIXTURE_NOW, FIXTURE_SENTINEL } from "./constants";

/**
 * The picture fixture mode shows, as named scenarios.
 *
 * Typed against `src/types.ts` so a field added to `TaskSummary` that this
 * seed does not follow fails `npm run typecheck`, rather than rendering an
 * `undefined` nobody notices. The seed is deliberately small and deliberately
 * ugly: one column with one card, another with twenty, a title that wraps, a
 * blocker with a long name — the states a layout change has to survive.
 */

export const SCENARIO_NAMES = [
  "busy",
  "one-run",
  "two-runs",
  "empty",
  "welcome",
  "error",
  // Task 017: one per state of the morning review, so each is one click (and a
  // key sequence) away for the screenshot script.
  "review-digest",
  "review-truncated",
  "review-pruned",
  "review-no-commits",
  "review-not-recorded",
  "review-chain",
  "review-empty",
  // Task 037: the review loop's states. One seed, exported under one name per
  // capture, so the screenshot names stay unique.
  "review-loop",
  "review-loop-panel",
  "review-loop-overlay",
  "review-loop-runs",
  "review-loop-analytics",
  "review-loop-settings",
  "review-loop-repository",
] as const;
export type ScenarioName = (typeof SCENARIO_NAMES)[number];

export interface Scenario {
  readonly name: ScenarioName;
  readonly appInfo: AppInfo;
  readonly repositories: Repository[];
  /** What `list_checkouts` answers: this computer's clone of each repository
   *  it has one of (task 066). A repository with none is not set up here. */
  readonly checkouts: CheckoutView[];
  /** What `list_local_worktrees` answers: where each task with a worktree on
   *  this computer has it (task 066). */
  readonly worktrees: LocalWorktree[];
  readonly tasks: TaskSummary[];
  /** Every run, newest first — the Runs view's history and each task's own. */
  readonly runs: RunListEntry[];
  /** One canned `runs:tail` payload per running run. */
  readonly tails: RunTail[];
  /** What `get_run` answers as each run's review, by run id. A run with no
   *  entry reads as `not_recorded`, like a row from before task 033. */
  readonly reviews: Record<string, RunReview>;
  /** What `get_review_digest` answers. Empty in every scenario but the one that
   *  shows a night. */
  readonly digest: ReviewDigest;
  /** What `list_review_findings` answers from, filtered by task and status.
   *  Empty in every scenario but `busy`, which holds one review's two
   *  findings, one open and one fixed, so the row exercises every field. */
  readonly reviewFindings: ReviewFinding[];
  /** What `get_review_history` answers, by task id. A task with no entry has
   *  no loops. Only the review-loop scenarios seed any. */
  readonly reviewHistories: Record<string, ReviewHistory>;
  /** The global review instructions and configuration. */
  readonly reviewSettings: ReviewSettings;
  /** What `get_review_level` answers per level: the level's own settings, what
   *  it inherits and what it resolves to. The seed states them; the fixture
   *  does not resolve a precedence chain of its own. */
  readonly reviewLevels: {
    readonly global: ReviewLevel;
    readonly repositories: Record<string, ReviewLevel>;
    readonly tasks: Record<string, ReviewLevel>;
  };
  /** `get_task`'s review instructions and configuration, by task id. */
  readonly taskReviews: Record<string, TaskReview>;
  /** `get_task_dependents` by task id. A task with no row falls back to the
   *  cards whose `blockingTitle` names it. */
  readonly dependents: Record<string, TaskDependent[]>;
  /** `get_task`'s `dependsOn` by task id. */
  readonly dependencies: Record<string, string[]>;
  /** What the local `get_diff_summary` answers, by task id: the branch as it is
   *  now, which the morning review must never show for a not-recorded run. */
  readonly liveDiffs: Record<string, DiffSummary>;
  readonly queueStatus: QueueStatus;
  readonly capacity: RunCapacity;
  readonly doctor: DoctorReport;
  readonly analytics: Analytics;
  /** When set, `list_tasks` for running tasks — the Runs view's first read —
   *  rejects with this. */
  readonly runsReadError: RimaiaError | null;
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NOW = Date.parse(FIXTURE_NOW);

/** An instant `ms` before {@link FIXTURE_NOW}, as the RFC 3339 the backend sends. */
function ago(ms: number): string {
  return new Date(NOW - ms).toISOString();
}

/** An instant `ms` after {@link FIXTURE_NOW}. */
function fromNow(ms: number): string {
  return new Date(NOW + ms).toISOString();
}

const REPO_APP = "repo-rimaia-app";
const REPO_SITE = "repo-marketing-site";

function repositories(): Repository[] {
  return [
    {
      id: REPO_APP,
      name: "rimaia-app",
      defaultBranch: "main",
      createdAt: ago(40 * DAY),
    },
    {
      id: REPO_SITE,
      name: "marketing-site",
      defaultBranch: "main",
      createdAt: ago(21 * DAY),
    },
  ];
}

/** This computer's clone of each of {@link repositories}, in the same order. */
function checkouts(): CheckoutView[] {
  return [
    {
      repositoryId: REPO_APP,
      path: "/Users/dev/code/rimaia-app",
      worktreeRoot: "/Users/dev/Library/Application Support/Rimaia/worktrees/rimaia-app",
      maxConcurrency: 3,
      unattendedConsent: true,
      onArchive: "remove_worktree",
      onArchiveScript: null,
    },
    {
      repositoryId: REPO_SITE,
      path: "/Users/dev/code/marketing-site",
      worktreeRoot: "/Users/dev/Library/Application Support/Rimaia/worktrees/marketing-site",
      maxConcurrency: 2,
      unattendedConsent: true,
      onArchive: "none",
      onArchiveScript: null,
    },
  ];
}

/** Where a seeded task's worktree is on this computer. */
function worktreeOf(taskId: string): LocalWorktree {
  return { taskId, path: `/worktrees/rimaia-app/${taskId}` };
}

interface TaskInit {
  id: string;
  title: string;
  column?: BoardColumn;
  repositoryId?: string;
  runState?: TaskSummary["runState"];
  lastRun?: TaskSummary["lastRun"];
  blockingTitle?: string | null;
  strategy?: "default" | "planned";
  links?: number;
}

let positionCounter = 0;

function task(init: TaskInit): TaskSummary {
  positionCounter += 1;
  const planned = init.strategy === "planned";
  return {
    id: init.id,
    repositoryId: init.repositoryId ?? REPO_APP,
    title: init.title,
    plan: `## Plan\n\n${init.title}\n\n- read the surrounding code first\n- keep the change small`,
    extraInstructions: null,
    column: init.column ?? "ready",
    position: positionCounter * 1024,
    runState: init.runState ?? "idle",
    branch: init.runState && init.runState !== "idle" ? `rimaia/${init.id}` : null,
    strategyMode: planned ? "planned" : "default",
    model: planned ? "opus" : null,
    effort: planned ? "high" : null,
    strategyPlan: null,
    strategySource: planned ? "planner" : null,
    strategyUpdatedAt: planned ? ago(2 * DAY) : null,
    createdAt: ago(10 * DAY),
    updatedAt: ago(3 * HOUR),
    source: "ui",
    archivedAt: null,
    effectiveModel: planned ? "opus" : null,
    effectiveEffort: planned ? "high" : null,
    effectiveOrigin: planned ? "task" : "claude_code",
    linkCount: init.links ?? 0,
    dependencyCount: init.blockingTitle ? 1 : 0,
    blockedByIncomplete: Boolean(init.blockingTitle),
    blockingTitle: init.blockingTitle ?? null,
    lastRun: init.lastRun ?? null,
    reviewLoop: null,
  };
}

function lastRun(
  status: RunStatus,
  exitClass: NonNullable<TaskSummary["lastRun"]>["exitClass"],
  endedAgo: number | null,
  resumeAfter: string | null = null,
): NonNullable<TaskSummary["lastRun"]> {
  return {
    // Every seeded row is an implementation run; the review loop's states
    // are task 037's to seed.
    kind: "implementation",
    status,
    exitClass,
    endedAt: endedAgo === null ? null : ago(endedAgo),
    resumeAfter,
  };
}

let runCounter = 0;

/** A history row for `taskSummary`, agreeing with its `lastRun`. */
function runFor(
  taskSummary: TaskSummary,
  repositoryName: string,
  startedAgo: number,
  overrides: Partial<Run> = {},
): RunListEntry {
  runCounter += 1;
  const summary = taskSummary.lastRun;
  const status = overrides.status ?? summary?.status ?? "succeeded";
  const running = status === "running";
  return {
    id: `run-${String(runCounter).padStart(3, "0")}`,
    taskId: taskSummary.id,
    attempt: 1,
    kind: "implementation",
    status,
    sessionId: `session-${runCounter}`,
    prompt: `Implement: ${taskSummary.title}`,
    startedAt: ago(startedAgo),
    endedAt: running ? null : (summary?.endedAt ?? ago(startedAgo - 25 * MINUTE)),
    exitClass: running ? null : (summary?.exitClass ?? "success"),
    errorMessage: status === "failed" ? "The agent stopped after repeated tool failures." : null,
    numTurns: running ? null : 38,
    costUsd: running ? null : 2.48,
    prUrl: status === "succeeded" ? `https://github.com/example/${repositoryName}/pull/${100 + runCounter}` : null,
    resumeAfter: summary?.resumeAfter ?? null,
    baseRef: "main",
    model: "claude-opus-4-1",
    effort: "high",
    runEnvironment: "inherit",
    inputTokens: 91_204,
    outputTokens: 14_880,
    cacheReadTokens: 1_204_331,
    cacheCreationTokens: 58_113,
    headSha: null,
    baseSha: null,
    taskTitle: taskSummary.title,
    repositoryId: taskSummary.repositoryId,
    repositoryName,
    logAvailable: true,
    ...overrides,
  };
}

function repoName(id: string): string {
  return id === REPO_SITE ? "marketing-site" : "rimaia-app";
}

function tailFor(run: RunListEntry, index: number): RunTail {
  const tools = [
    { id: "tool-1", name: "Edit", detail: "src/components/board/TaskCard.tsx" },
    { id: "tool-2", name: "Bash", detail: "npm run typecheck" },
    { id: "tool-3", name: "Read", detail: "src/styles/board.css" },
  ];
  const texts = [
    "The card's badge reuses the status colour tokens, so I will keep the contrast check on the new pill.",
    "Typecheck is clean; running the unit tests for the board reducer next.",
    "I read the stylesheet end to end. The column header and the count share one flex row, which is why they collide at 1024px.",
  ];
  return {
    runId: run.id,
    elapsedMs: (7 + index * 11) * MINUTE + 14_000,
    turns: 12 + index * 9,
    currentTool: tools[index % tools.length],
    lastAssistantText: texts[index % texts.length],
  };
}

function doctor(): DoctorReport {
  const results: DoctorCheckResult[] = [
    {
      check: "claude_cli",
      label: "Claude Code CLI",
      repository: null,
      status: "pass",
      detail: "claude 2.1.234 found at /usr/local/bin/claude",
      remediation: null,
      dismissed: false,
    },
    {
      check: "git",
      label: "git",
      repository: null,
      status: "pass",
      detail: "git 2.50.1",
      remediation: null,
      dismissed: false,
    },
    {
      check: "github_cli",
      label: "GitHub CLI",
      repository: null,
      status: "warn",
      detail: "gh is installed but not signed in, so runs cannot open pull requests.",
      remediation: "Run `gh auth login` in a terminal, then check again.",
      dismissed: false,
    },
    {
      check: "disk_space",
      label: "Free disk space",
      repository: null,
      status: "warn",
      detail: "4.2 GB free on the volume holding the worktrees.",
      remediation: "Remove finished worktrees from Settings, Storage.",
      dismissed: true,
    },
    {
      check: "repository_path",
      label: "Repository path",
      repository: "marketing-site",
      status: "fail",
      detail: "/Users/dev/code/marketing-site no longer exists.",
      remediation: "Move the repository back, or remove it from Settings and add it again.",
      dismissed: false,
    },
  ];
  return {
    results,
    dismissals: [
      {
        check: "disk_space",
        repository: null,
        detail: "4.2 GB free on the volume holding the worktrees.",
      },
    ],
  };
}

function healthyDoctor(): DoctorReport {
  const results = doctor().results.map((result) => ({
    ...result,
    status: "pass" as const,
    detail: `${result.label} is ready.`,
    remediation: null,
    dismissed: false,
  }));
  return { results: results.slice(0, 4), dismissals: [] };
}

function analytics(withData: boolean): Analytics {
  if (!withData) {
    return {
      period: { from: null, to: null },
      outcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 0 },
      spendUsd: 0,
      spendByDay: [],
      runsWithoutCost: 0,
      runsWithoutModel: 0,
      tasksAttempted: 0,
      tasksCompleted: 0,
      costPerCompletedTaskUsd: null,
      medianDurationSeconds: null,
      longestRun: null,
      unattendedHours: 0,
      models: [],
      strategies: [],
      plannerSpendUsd: 0,
      implementationSpendUsd: 0,
      reviewLoopSpendUsd: 0,
      reviewLoopOutcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 0 },
      subscriptionMonthlyUsd: null,
    };
  }
  const spend = [3.1, 0, 7.45, 12.9, 4.02, 9.6, 5.35];
  return {
    period: { from: ago(7 * DAY), to: fromNow(0) },
    outcomes: { succeeded: 21, failed: 4, cancelled: 2, interrupted: 1, running: 3 },
    spendUsd: spend.reduce((total, day) => total + day, 0),
    spendByDay: spend.map((spendUsd, index) => ({
      day: new Date(NOW - (6 - index) * DAY).toISOString().slice(0, 10),
      spendUsd,
      runs: spendUsd === 0 ? 0 : 3 + index,
    })),
    runsWithoutCost: 2,
    runsWithoutModel: 1,
    tasksAttempted: 24,
    tasksCompleted: 18,
    costPerCompletedTaskUsd: 2.62,
    medianDurationSeconds: 1_440,
    longestRun: {
      runId: "run-004",
      taskId: "t-ready-02",
      title: "Replace the hand-rolled date picker with the platform input",
      seconds: 5_820,
    },
    unattendedHours: 14.5,
    models: [
      { model: "claude-opus-4-1", runs: 19, spendUsd: 38.2 },
      { model: "claude-sonnet-4", runs: 11, spendUsd: 4.22 },
    ],
    strategies: [
      { mode: "default", runs: 17, spendUsd: 21.4 },
      { mode: "planned", runs: 11, spendUsd: 18.9 },
      { mode: "manual", runs: 2, spendUsd: 2.12 },
    ],
    plannerSpendUsd: 3.4,
    implementationSpendUsd: 39.02,
    reviewLoopSpendUsd: 0,
    reviewLoopOutcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 0 },
    subscriptionMonthlyUsd: 200,
  };
}

function queueFor(
  tasks: TaskSummary[],
  state: QueueStatus["state"],
  running: TaskSummary[],
): QueueStatus {
  const plan: QueueEntry[] = [];
  let position = 1;
  for (const candidate of tasks) {
    if (candidate.column !== "ready" || candidate.runState === "running") continue;
    let skip: QueueEntry["skip"] = null;
    if (candidate.runState === "blocked") skip = "dependency_not_satisfied";
    else if (candidate.runState === "waiting_retry") skip = "waiting_for_retry";
    else if (candidate.runState === "failed" || candidate.runState === "cancelled") {
      skip = "needs_attention";
    }
    plan.push({
      taskId: candidate.id,
      title: candidate.title,
      repositoryId: candidate.repositoryId,
      queuePosition: skip === null ? position++ : null,
      skip,
      resumeAfter: candidate.lastRun?.resumeAfter ?? null,
    });
  }
  return {
    state,
    runningTaskIds: running.map((candidate) => candidate.id),
    plan,
    lastStepError: null,
    usageLimitPauseUntil: null,
    window: null,
  };
}

const IDLE_READY_TITLES = [
  "Make the board column header and count survive a narrow window",
  "Add keyboard shortcuts to the task detail panel",
  "Show the repository name on cards when the filter is All",
  "Stop the sidebar tagline from truncating at 1024px",
  "Group the Settings index by what it changes, not by when it shipped",
  "Give the empty Runs view a sentence about what to do next",
  "Use tabular numerals for every cost on the Runs view",
  "Persist the board's repository filter across restarts",
  "Add a Copy branch name button to the worktree section",
  `Rename the Done column's empty state copy [${FIXTURE_SENTINEL}]`,
  "Rework the archive list so restoring a card does not need a second click",
];

const LONG_TITLE =
  "Audit every stylesheet for hard-coded colours and replace them with the design tokens, keeping dark and light in step";

/** Everything a scenario with a board and a Runs view is made of; the four
 *  scenarios that look like a working installation differ only in how many
 *  cards there are and how many are running. */
function populated(name: ScenarioName, runningCount: number, full: boolean): Scenario {
  positionCounter = 0;
  runCounter = 0;

  const runningTitles = [
    "Fix the settings index jump links so they land below the sticky header",
    "Replace the hand-rolled date picker with the platform input",
    "Make the analytics bar chart readable in the light theme",
  ];
  const runningRepos = [REPO_APP, REPO_APP, REPO_SITE];
  const running = runningTitles.slice(0, runningCount).map((title, index) =>
    task({
      id: `t-running-${index + 1}`,
      title,
      repositoryId: runningRepos[index],
      runState: "running",
      lastRun: lastRun("running", null, null),
      strategy: index === 0 ? "planned" : "default",
    }),
  );

  const others: TaskSummary[] = [];
  const ready: TaskSummary[] = [];
  if (full) {
    ready.push(
      task({
        id: "t-queued",
        title: "Add a retry button to the failed-run banner",
        runState: "queued",
      }),
      task({
        id: "t-waiting",
        title: "Cache the repository lookup between board renders",
        runState: "waiting_retry",
        lastRun: lastRun("failed", "usage_limit", 20 * MINUTE, fromNow(2 * HOUR + 12 * MINUTE)),
      }),
      task({
        id: "t-failed",
        title: "Migrate the analytics page to the shared period selector",
        runState: "failed",
        lastRun: lastRun("failed", "fatal", 5 * HOUR),
      }),
      task({
        id: "t-cancelled",
        title: "Prototype drag handles for the dependency editor",
        runState: "cancelled",
        lastRun: lastRun("cancelled", "cancelled", 9 * HOUR),
      }),
      task({
        id: "t-interrupted",
        title: "Split the task detail panel into tabs",
        runState: "failed",
        lastRun: lastRun("interrupted", "interrupted", 11 * HOUR),
      }),
      task({
        id: "t-blocked",
        title: "Wire the new status colours into the run history list",
        runState: "blocked",
        blockingTitle:
          "Introduce semantic status tokens for success, warning and danger across both themes",
      }),
      task({ id: "t-long-title", title: LONG_TITLE, links: 2 }),
    );
    IDLE_READY_TITLES.forEach((title, index) =>
      ready.push(task({ id: `t-ready-${String(index + 1).padStart(2, "0")}`, title })),
    );
    // 3 running + 7 above + 11 idle = 21; the long title is the 20th ready card
    // once the running ones are counted, so drop one idle title to land on 20.
    ready.pop();

    others.push(
      ...[
        "Investigate the flaky board drag test",
        "Decide how cards should show three dependencies",
        "Draft the keyboard map for the Runs view",
        "Sketch a compact density for the board",
        "Write down what the review queue should show first",
        "Check the welcome screen against the new doctor wording",
      ].map((title, index) =>
        task({
          id: `t-notready-${index + 1}`,
          title,
          column: "not_ready",
          strategy: index === 1 ? "planned" : "default",
        }),
      ),
      ...[
        ["Add the doctor banner to every view", 101],
        ["Show the queue plan on the Runs view", 102],
        ["Let the board filter by run state", 103],
      ].map(([title, pr], index) =>
        task({
          id: `t-review-${index + 1}`,
          title: title as string,
          column: "in_review",
          lastRun: lastRun("succeeded", "success", (3 + index) * HOUR),
          links: pr === 102 ? 1 : 0,
        }),
      ),
      task({
        id: "t-done-1",
        title: "Record the first unattended run's findings",
        column: "done",
        lastRun: lastRun("succeeded", "success", 2 * DAY),
      }),
    );
  } else {
    ready.push(
      task({ id: "t-ready-01", title: IDLE_READY_TITLES[0] }),
      task({ id: "t-ready-02", title: IDLE_READY_TITLES[1] }),
    );
    others.push(
      task({ id: "t-notready-1", title: "Draft the keyboard map for the Runs view", column: "not_ready" }),
      task({
        id: "t-done-1",
        title: "Record the first unattended run's findings",
        column: "done",
        lastRun: lastRun("succeeded", "success", 2 * DAY),
      }),
    );
  }

  const tasks = [...running, ...ready, ...others];
  const byId = new Map(tasks.map((candidate) => [candidate.id, candidate]));

  const runs: RunListEntry[] = [];
  running.forEach((candidate, index) => {
    runs.push(runFor(candidate, repoName(candidate.repositoryId), (7 + index * 11) * MINUTE));
  });
  const history: Array<[string, number]> = full
    ? [
        ["t-failed", 5 * HOUR + 40 * MINUTE],
        ["t-waiting", 20 * MINUTE + 31 * MINUTE],
        ["t-review-1", 3 * HOUR + 50 * MINUTE],
        ["t-review-2", 4 * HOUR + 30 * MINUTE],
        ["t-review-3", 5 * HOUR + 10 * MINUTE],
        ["t-cancelled", 9 * HOUR + 10 * MINUTE],
        ["t-interrupted", 11 * HOUR + 20 * MINUTE],
        ["t-done-1", 2 * DAY + 45 * MINUTE],
      ]
    : [["t-done-1", 2 * DAY + 45 * MINUTE]];
  for (const [id, startedAgo] of history) {
    const candidate = byId.get(id);
    if (candidate) {
      runs.push(runFor(candidate, repoName(candidate.repositoryId), startedAgo));
    }
  }

  const tails = running.map((candidate, index) => {
    const run = runs.find((entry) => entry.taskId === candidate.id);
    return tailFor(run as RunListEntry, index);
  });

  // Task 033: every finished, successful run in the busy picture recorded a
  // bundle, and it is the truncated kind, so the run detail screenshot shows
  // the per-file markers and the pointer to the pull request.
  const reviews: Record<string, RunReview> = {};
  if (full) {
    for (const run of runs) {
      if (run.status === "succeeded" && run.endedAt) {
        const bundle = truncatedBundle(run.endedAt);
        reviews[run.id] = { source: "recorded", bundle };
        run.headSha = bundle.commits[0].sha;
        run.baseSha = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
      }
    }
  }

  const parallel = runningCount >= 2;
  const maxConcurrency = parallel ? Math.max(runningCount, 3) : 1;
  return {
    name,
    appInfo: appInfo(true),
    repositories: repositories(),
    checkouts: checkouts(),
    worktrees: [],
    tasks,
    runs,
    tails,
    reviews,
    ...noReviewExtras(),
    reviewFindings: full ? reviewFindingsFor("t-review-1") : [],
    queueStatus: queueFor(tasks, "running", running),
    capacity: { mode: parallel ? "parallel" : "sequential", maxConcurrency, ceiling: 8 },
    doctor: full ? doctor() : healthyDoctor(),
    analytics: analytics(full),
    runsReadError: null,
  };
}

/** A recorded bundle whose regenerated lockfile outgrew the patch cap, beside
 *  a binary screenshot and the source change a reviewer actually wants. */
function truncatedBundle(createdAt: string): StoredBundle {
  const source = [
    "diff --git a/src/components/DoctorBanner.tsx b/src/components/DoctorBanner.tsx",
    "new file mode 100644",
    "index 0000000..4be1c2a",
    "--- /dev/null",
    "+++ b/src/components/DoctorBanner.tsx",
    "@@ -0,0 +1,12 @@",
    '+import type { DoctorReport } from "../types";',
    "+",
    "+/** One calm line above every view while a check is failing. */",
    "+export function DoctorBanner({ report }: { report: DoctorReport }) {",
    '+  const failing = report.results.filter((result) => result.status === "fail");',
    "+  if (failing.length === 0) return null;",
    "+  return (",
    '+    <p className="doctor-banner" role="status">',
    "+      {failing.length} setup check{failing.length === 1 ? \"\" : \"s\"} need attention.",
    "+    </p>",
    "+  );",
    "+}",
    "diff --git a/src/App.tsx b/src/App.tsx",
    "index 9d1e0f3..a7c4b21 100644",
    "--- a/src/App.tsx",
    "+++ b/src/App.tsx",
    "@@ -41,6 +41,7 @@ export function App() {",
    "   return (",
    '     <div className="app">',
    "       <Sidebar view={view} onNavigate={setView} />",
    "+      <DoctorBanner report={doctor} />",
    "       <main>{content}</main>",
    "     </div>",
    "   );",
    "",
  ].join("\n");
  return {
    diff: { filesChanged: 4, insertions: 9_431, deletions: 2_180 },
    files: [
      { path: "package-lock.json", insertions: 9_412, deletions: 2_179, patch: "too_large" },
      { path: "docs/doctor-banner.png", insertions: null, deletions: null, patch: "binary" },
      { path: "src/App.tsx", insertions: 1, deletions: 0, patch: "included" },
      { path: "src/components/DoctorBanner.tsx", insertions: 12, deletions: 0, patch: "included" },
    ],
    commits: [
      {
        sha: "9f2c1d47a8e3b5f60c1d2e3f4a5b6c7d8e9f0a1b",
        shortSha: "9f2c1d4",
        subject: "Mount the doctor banner above every view",
        author: "Rimaia",
        committedAt: createdAt,
      },
      {
        sha: "4be1c2a9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3",
        shortSha: "4be1c2a",
        subject: "Add the doctor banner component",
        author: "Rimaia",
        committedAt: createdAt,
      },
      {
        sha: "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
        shortSha: "1a2b3c4",
        subject: "Bump the lockfile for the icon package",
        author: "Rimaia",
        committedAt: createdAt,
      },
    ],
    patch: source,
    patchBytes: 1_948_312,
    patchTruncated: true,
    patchPrunedAt: null,
    createdAt,
  };
}

/** The digest no scenario but `review-digest` can contradict: nothing ended in
 *  the 24-hour window that ends at {@link FIXTURE_NOW}. */
function emptyDigest(): ReviewDigest {
  return {
    since: ago(DAY),
    until: FIXTURE_NOW,
    entries: [],
    totals: {
      runs: 0,
      runSeconds: 0,
      spanSeconds: null,
      costUsd: 0,
      runsWithoutCost: 0,
      counts: {
        failed: 0,
        blocked: 0,
        waiting_retry: 0,
        interrupted: 0,
        cancelled: 0,
        running: 0,
        completed: 0,
        skipped: 0,
      },
    },
  };
}

/** What a level inherits when nothing above it says anything: the built-in
 *  defaults (task 021). */
export const BUILT_IN_REVIEW: ReviewConfig = {
  enabled: "off",
  max_review_loops: 2,
  blocking_severity: "medium",
  fix_session: "fresh",
};

/** A level whose own settings are `config` over `inherited`. */
export function reviewLevel(config: ReviewConfig, inherited: ReviewConfig = BUILT_IN_REVIEW): ReviewLevel {
  return { config, inherited, effective: { ...inherited, ...config } };
}

function noReviewExtras(): Pick<
  Scenario,
  | "digest"
  | "dependents"
  | "dependencies"
  | "liveDiffs"
  | "reviewFindings"
  | "reviewHistories"
  | "reviewSettings"
  | "reviewLevels"
  | "taskReviews"
> {
  return {
    digest: emptyDigest(),
    dependents: {},
    dependencies: {},
    liveDiffs: {},
    reviewFindings: [],
    reviewHistories: {},
    reviewSettings: { instructions: "", config: {} },
    reviewLevels: { global: reviewLevel({}), repositories: {}, tasks: {} },
    taskReviews: {},
  };
}

/** One review run's report on `taskId`: an open finding about one line, and a
 *  fixed one about the change as a whole with the fix run's resolution. */
function reviewFindingsFor(taskId: string): ReviewFinding[] {
  const reviewRunId = `run-review-for-${taskId}`;
  return [
    {
      id: `finding-${taskId}-0`,
      taskId,
      reviewRunId,
      ordinal: 0,
      severity: "high",
      title: "The banner hides the queue controls on narrow windows",
      body: "Below 900px the banner overlaps the Start and Pause buttons, so a failing check also blocks the one action that would fix it.",
      file: "src/components/DoctorBanner.tsx",
      line: 9,
      fingerprint: null,
      status: "open",
      resolution: null,
      resolvedByRunId: null,
      createdAt: ago(2 * HOUR),
      resolvedAt: null,
    },
    {
      id: `finding-${taskId}-1`,
      taskId,
      reviewRunId,
      ordinal: 1,
      severity: "low",
      title: "No test covers the banner's dismissed state",
      body: "The dismissed branch renders nothing, and nothing asserts it.",
      file: null,
      line: null,
      fingerprint: null,
      status: "fixed",
      resolution: "Added a test that dismisses the warning and asserts the banner is gone.",
      resolvedByRunId: `run-fix-for-${taskId}`,
      createdAt: ago(2 * HOUR),
      resolvedAt: ago(90 * MINUTE),
    },
  ];
}

/** A small change that fits the patch whole: two files, one commit. */
function smallBundle(createdAt: string, subject: string): StoredBundle {
  const patch = [
    "diff --git a/src/lib/board.ts b/src/lib/board.ts",
    "index 3c1d9aa..e07b5c2 100644",
    "--- a/src/lib/board.ts",
    "+++ b/src/lib/board.ts",
    "@@ -88,7 +88,8 @@ export function groupIntoColumns<T extends BoardCard>(tasks: readonly T[]) {",
    "   for (const column of BOARD_COLUMNS) {",
    "-    grouped[column].sort(compareBoardOrder);",
    "+    // Each repository's cards stay together: a position is only comparable inside one.",
    "+    grouped[column].sort(compareBoardOrder);",
    "   }",
    "   return grouped;",
    " }",
    "diff --git a/src/lib/board.test.ts b/src/lib/board.test.ts",
    "index 91f0b3d..2a6e4f8 100644",
    "--- a/src/lib/board.test.ts",
    "+++ b/src/lib/board.test.ts",
    "@@ -12,3 +12,9 @@ describe(\"groupIntoColumns\", () => {",
    "+  it(\"keeps one repository's cards together\", () => {",
    "+    const grouped = groupIntoColumns([card(\"a\", 1, \"x\"), card(\"b\", 2, \"y\"), card(\"c\", 3, \"x\")]);",
    "+    expect(grouped.ready.map((c) => c.id)).toEqual([\"a\", \"c\", \"b\"]);",
    "+  });",
    "",
  ].join("\n");
  return {
    diff: { filesChanged: 2, insertions: 8, deletions: 1 },
    files: [
      { path: "src/lib/board.ts", insertions: 2, deletions: 1, patch: "included" },
      { path: "src/lib/board.test.ts", insertions: 6, deletions: 0, patch: "included" },
    ],
    commits: [
      {
        sha: "7d3e5f60a1b2c3d4e5f60718293a4b5c6d7e8f90",
        shortSha: "7d3e5f6",
        subject,
        author: "Rimaia",
        committedAt: createdAt,
      },
    ],
    patch,
    patchBytes: patch.length,
    patchTruncated: false,
    patchPrunedAt: null,
    createdAt,
  };
}

/** The same kind of change once the patch has aged out (ADR-0036 point 6): the
 *  file list and commits are kept, the patch is not. */
function prunedBundle(createdAt: string): StoredBundle {
  return {
    ...smallBundle(createdAt, "Keep each repository's cards together"),
    patch: null,
    patchPrunedAt: ago(2 * DAY),
  };
}

interface ReviewSpec {
  readonly id: string;
  readonly title: string;
  /** What `get_run` answers for the task's newest run; `"none"` is a card
   *  dragged into review by hand, which has no run at all. */
  readonly review: RunReview | "none";
  readonly pr?: boolean;
}

function reviewTask(spec: ReviewSpec, index: number): TaskSummary {
  const base = task({
    id: spec.id,
    title: spec.title,
    column: "in_review",
    lastRun: spec.review === "none" ? null : lastRun("succeeded", "success", (3 + index) * HOUR),
  });
  return { ...base, branch: `rimaia/${spec.id}` };
}

/** The tasks around the reviewed ones, so the board is not only the queue. */
function reviewSurroundings(): TaskSummary[] {
  return [
    task({ id: "t-ready-01", title: IDLE_READY_TITLES[0] }),
    task({ id: "t-ready-02", title: IDLE_READY_TITLES[1] }),
    task({
      id: "t-done-1",
      title: "Record the first unattended run's findings",
      column: "done",
      lastRun: lastRun("succeeded", "success", 2 * DAY),
    }),
  ];
}

interface ReviewBoardOptions {
  readonly dependents?: Record<string, TaskDependent[]>;
  readonly dependencies?: Record<string, string[]>;
  readonly liveDiffs?: Record<string, DiffSummary>;
}

/** A board whose `in_review` column holds `specs`, the first of them first in
 *  board order — so the queue opens on the state the scenario is named for. */
function reviewBoard(
  name: ScenarioName,
  specs: ReviewSpec[],
  options: ReviewBoardOptions = {},
): Scenario {
  positionCounter = 0;
  runCounter = 0;
  const reviewed = specs.map(reviewTask);
  const tasks = [...reviewed, ...reviewSurroundings()];

  const runs: RunListEntry[] = [];
  const reviews: Record<string, RunReview> = {};
  specs.forEach((spec, index) => {
    if (spec.review === "none") return;
    const summary = reviewed[index];
    const run = runFor(summary, "rimaia-app", (3 + index) * HOUR + 40 * MINUTE, {
      prUrl: spec.pr === false ? null : `https://github.com/example/rimaia-app/pull/${210 + index}`,
    });
    runs.push(run);
    if (spec.review.source === "recorded") {
      reviews[run.id] = spec.review;
      if (spec.review.bundle) {
        run.headSha = spec.review.bundle.commits[0].sha;
        run.baseSha = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
      } else {
        run.headSha = run.baseSha = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
      }
    }
  });

  return {
    name,
    appInfo: appInfo(true),
    repositories: repositories(),
    checkouts: checkouts(),
    worktrees: reviewed.map((summary) => worktreeOf(summary.id)),
    tasks,
    runs,
    tails: [],
    reviews,
    ...noReviewExtras(),
    dependents: options.dependents ?? {},
    dependencies: options.dependencies ?? {},
    liveDiffs: options.liveDiffs ?? {},
    queueStatus: queueFor(tasks, "paused", []),
    capacity: { mode: "sequential", maxConcurrency: 1, ceiling: 8 },
    doctor: healthyDoctor(),
    analytics: analytics(false),
    runsReadError: null,
  };
}

const QUEUE_TAIL: ReviewSpec[] = [
  {
    id: "t-review-2",
    title: "Show the queue plan on the Runs view",
    review: { source: "recorded", bundle: smallBundle(ago(4 * HOUR), "Show the queue plan") },
  },
  {
    id: "t-review-3",
    title: "Let the board filter by run state",
    review: { source: "recorded", bundle: null },
  },
];

function reviewChain(): Scenario {
  const reviewed: ReviewSpec = {
    id: "t-review-1",
    title: "Introduce semantic status tokens for success, warning and danger across both themes",
    review: {
      source: "recorded",
      bundle: smallBundle(ago(3 * HOUR), "Add the semantic status tokens"),
    },
  };
  const dependent = (
    id: string,
    title: string,
    overrides: Partial<TaskDependent> = {},
  ): TaskDependent => ({
    id,
    title,
    column: "ready",
    runState: "blocked",
    archivedAt: null,
    builtOn: false,
    ...overrides,
  });
  return reviewBoard("review-chain", [reviewed, ...QUEUE_TAIL], {
    dependencies: { [reviewed.id]: ["t-done-1"] },
    dependents: {
      [reviewed.id]: [
        dependent("t-dep-1", "Wire the new status colours into the run history list", {
          builtOn: true,
          column: "in_review",
          runState: "idle",
        }),
        dependent("t-dep-2", "Give the digest outcomes the same dot and word as the board"),
        dependent("t-dep-3", "Retire the legacy badge pairs", {
          column: "not_ready",
          runState: "idle",
          archivedAt: ago(DAY),
        }),
      ],
    },
  });
}

function reviewDigestScenario(): Scenario {
  const base = reviewBoard("review-digest", [
    {
      id: "t-review-1",
      title: "Add the doctor banner to every view",
      review: { source: "recorded", bundle: smallBundle(ago(3 * HOUR), "Add the doctor banner") },
    },
    {
      id: "t-review-2",
      title: "Show the queue plan on the Runs view",
      review: { source: "recorded", bundle: smallBundle(ago(4 * HOUR), "Show the queue plan") },
    },
  ]);
  const failedTitle = "Migrate the analytics page to the shared period selector";
  const entry = (
    taskId: string,
    title: string,
    outcome: DigestEntry["outcome"],
    overrides: Partial<DigestEntry> = {},
  ): DigestEntry => ({
    taskId,
    title,
    repositoryId: REPO_APP,
    column: outcome === "completed" ? "in_review" : "ready",
    outcome,
    runs: 0,
    runSeconds: null,
    costUsd: null,
    lastRunId: null,
    errorMessage: null,
    prUrl: null,
    blockingTitle: null,
    lastRunKind: outcome === "blocked" || outcome === "skipped" ? null : "implementation",
    reviewLoop: null,
    skipReason: null,
    ...overrides,
  });
  return {
    ...base,
    digest: {
      since: ago(11 * HOUR),
      until: FIXTURE_NOW,
      entries: [
        entry("t-failed", failedTitle, "failed", {
          runs: 2,
          runSeconds: 2_460,
          costUsd: 3.18,
          errorMessage: "The agent stopped after repeated tool failures.",
        }),
        entry("t-blocked-1", "Wire the new status colours into the run history list", "blocked", {
          blockingTitle: failedTitle,
        }),
        entry("t-blocked-2", "Retire the legacy badge styles", "blocked", {
          blockingTitle: failedTitle,
        }),
        entry("t-waiting", "Cache the repository lookup between board renders", "waiting_retry", {
          runs: 1,
          runSeconds: 1_860,
          costUsd: 1.12,
        }),
        entry("t-cancelled", "Prototype drag handles for the dependency editor", "cancelled", {
          runs: 1,
          runSeconds: 540,
          costUsd: 0.46,
        }),
        entry("t-review-1", "Add the doctor banner to every view", "completed", {
          runs: 1,
          runSeconds: 1_980,
          costUsd: 2.48,
        }),
        // The run whose cost was never recorded: "not recorded", never $0.00.
        entry("t-review-2", "Show the queue plan on the Runs view", "completed", {
          runs: 1,
          runSeconds: 1_560,
          costUsd: null,
        }),
        entry("t-skipped", "Refresh the marketing site footer", "skipped", {
          repositoryId: REPO_SITE,
          skipReason: "unattended_runs_not_allowed",
        }),
      ],
      totals: {
        runs: 6,
        runSeconds: 8_400,
        spanSeconds: 27_000,
        costUsd: 7.24,
        runsWithoutCost: 1,
        counts: {
          failed: 1,
          blocked: 2,
          waiting_retry: 1,
          interrupted: 0,
          cancelled: 1,
          running: 0,
          completed: 2,
          skipped: 1,
        },
      },
    },
  };
}

// ---------------------------------------------------------------------------
// The review loop (task 037)
// ---------------------------------------------------------------------------

const REVIEW_LOOP_ON: ReviewConfig = { enabled: "on_cost_acknowledged", max_review_loops: 3 };

function loopSummary(overrides: Partial<ReviewLoopSummary> = {}): ReviewLoopSummary {
  return {
    enabled: true,
    maxReviewLoops: 2,
    fixesSpent: 0,
    reviews: 1,
    verdict: { verdict: "clean" },
    openBlocking: 0,
    openAdvisory: 0,
    pingPong: false,
    ...overrides,
  };
}

function lastRunOf(
  kind: RunKind,
  status: RunStatus,
  exitClass: NonNullable<TaskSummary["lastRun"]>["exitClass"],
  endedAgo: number | null,
  resumeAfter: string | null = null,
): NonNullable<TaskSummary["lastRun"]> {
  return { ...lastRun(status, exitClass, endedAgo, resumeAfter), kind };
}

/** One card whose loop is in the state the title names. Every one is a card a
 *  real loop can produce: a finished loop lands in `in_review` and `idle`, so
 *  the card has no badge and the verdict line carries it; a loop still moving
 *  sits in `ready` with the badge and no verdict line. */
interface LoopCard {
  readonly id: string;
  readonly title: string;
  readonly last: NonNullable<TaskSummary["lastRun"]>;
  readonly loop: ReviewLoopSummary;
  readonly moving?: TaskSummary["runState"];
}

const LOOP_CARDS: readonly LoopCard[] = [
  {
    // The card the panel and overlay captures open.
    id: "rl-circles",
    title: "Rework the login redirect after sign-out",
    last: lastRunOf("review", "succeeded", "success", 2 * HOUR),
    loop: loopSummary({
      fixesSpent: 1,
      reviews: 2,
      verdict: { verdict: "findings_remain", openBlocking: 2 },
      openBlocking: 2,
      openAdvisory: 1,
      pingPong: true,
    }),
  },
  {
    id: "rl-clean0",
    title: "Show the repository name on cards when the filter is All",
    last: lastRunOf("review", "succeeded", "success", 3 * HOUR),
    loop: loopSummary(),
  },
  {
    id: "rl-clean2",
    title: "Persist the board's repository filter across restarts",
    last: lastRunOf("review", "succeeded", "success", 4 * HOUR),
    loop: loopSummary({ fixesSpent: 2, reviews: 3 }),
  },
  {
    id: "rl-remain1",
    title: "Stop the sidebar tagline from truncating at 1024px",
    last: lastRunOf("review", "succeeded", "success", 5 * HOUR),
    loop: loopSummary({
      verdict: { verdict: "findings_remain", openBlocking: 1 },
      openBlocking: 1,
    }),
  },
  {
    id: "rl-remain3",
    title: "Group the Settings index by what it changes, not by when it shipped",
    last: lastRunOf("review", "succeeded", "success", 6 * HOUR),
    loop: loopSummary({
      fixesSpent: 2,
      reviews: 3,
      verdict: { verdict: "findings_remain", openBlocking: 3 },
      openBlocking: 3,
      openAdvisory: 2,
    }),
  },
  {
    id: "rl-failed",
    title: "Add keyboard shortcuts to the task detail panel",
    last: lastRunOf("review", "failed", "fatal", 7 * HOUR),
    loop: loopSummary({ verdict: { verdict: "unreviewed", reason: "review_failed" } }),
  },
  {
    id: "rl-silent",
    title: "Use tabular numerals for every cost on the Runs view",
    last: lastRunOf("review", "succeeded", "success", 8 * HOUR),
    loop: loopSummary({ verdict: { verdict: "unreviewed", reason: "nothing_recorded" } }),
  },
  {
    id: "rl-moved",
    title: "Give the empty Runs view a sentence about what to do next",
    last: lastRunOf("review", "succeeded", "success", 9 * HOUR),
    loop: loopSummary({ verdict: { verdict: "unreviewed", reason: "review_changed_branch" } }),
  },
  {
    id: "rl-fixlast",
    title: "Add a Copy branch name button to the worktree section",
    last: lastRunOf("fix", "succeeded", "success", 10 * HOUR),
    loop: loopSummary({
      fixesSpent: 1,
      verdict: { verdict: "unreviewed", reason: "fix_not_reviewed" },
    }),
  },
  {
    id: "rl-later",
    title: "Rework the archive list so restoring a card does not need a second click",
    last: lastRunOf("implementation", "succeeded", "success", 11 * HOUR),
    loop: loopSummary({
      reviews: 0,
      verdict: { verdict: "unreviewed", reason: "not_reviewed" },
    }),
  },
  {
    // In flight: the badge says what is happening and the verdict waits.
    id: "rl-reviewing",
    title: "Make the board column header and count survive a narrow window",
    last: lastRunOf("review", "running", null, null),
    loop: loopSummary({ verdict: { verdict: "unreviewed", reason: "not_reviewed" } }),
    moving: "running",
  },
  {
    id: "rl-waiting",
    title: "Cache the repository lookup between board renders",
    last: lastRunOf("review", "failed", "usage_limit", 25 * MINUTE, fromNow(2 * HOUR + 12 * MINUTE)),
    loop: loopSummary({ verdict: { verdict: "unreviewed", reason: "not_reviewed" } }),
    moving: "waiting_retry",
  },
];

const CIRCLES = "rl-circles";

/** The attempts of the task the panel opens, as one ascending sequence
 *  (seam-contract D29 point 2): an earlier loop of implementation #1, review
 *  #2 and fix #3, then the newest of implementation #4, review #5, fix #6 and
 *  review #7. */
const CIRCLES_ATTEMPTS: ReadonlyArray<readonly [number, RunKind]> = [
  [1, "implementation"],
  [2, "review"],
  [3, "fix"],
  [4, "implementation"],
  [5, "review"],
  [6, "fix"],
  [7, "review"],
];

const circlesRunId = (attempt: number) => `rl-circles-run-${attempt}`;

function circlesHistory(): ReviewHistory {
  const finding = (
    id: string,
    reviewAttempt: number,
    ordinal: number,
    overrides: Partial<ReviewHistory["loops"][number]["rounds"][number]["findings"][number]>,
  ) => ({
    id: `rl-circles-${id}`,
    taskId: CIRCLES,
    reviewRunId: circlesRunId(reviewAttempt),
    ordinal,
    severity: "high" as const,
    title: "",
    body: "",
    file: "src/auth/redirect.ts" as string | null,
    line: 41 as number | null,
    fingerprint: `src/auth/redirect.ts|${id}` as string | null,
    status: "open" as const,
    resolution: null as string | null,
    resolvedByRunId: null as string | null,
    createdAt: ago(5 * HOUR),
    resolvedAt: null as string | null,
    blocking: true,
    carriedOver: false,
    ...overrides,
  });
  const phase = (kind: RunKind, attempts: number[], status: RunStatus = "succeeded") => ({
    kind,
    runIds: attempts.map(circlesRunId),
    attempts,
    status,
    exitClass: "success" as const,
  });

  const fixedByFirstFix = {
    status: "fixed" as const,
    resolution: "Validated the target against the allow-list and added a test.",
    resolvedByRunId: circlesRunId(6),
    resolvedAt: ago(3 * HOUR),
  };
  const redirect = finding("redirect", 5, 0, {
    title: "Redirect target is not validated",
    body: "The sign-out redirect follows whatever `returnTo` says, so a crafted link can send a signed-out user to another site.",
    ...fixedByFirstFix,
  });
  const cookie = finding("cookie", 5, 1, {
    title: "Session cookie survives sign-out",
    body: "Sign-out clears the server session but leaves the cookie in the browser, so the next request is treated as an expired session instead of a fresh visit.",
    file: "src/auth/session.ts",
    line: 77,
    ...fixedByFirstFix,
    resolution: "Cleared the cookie on the sign-out response.",
  });
  const timeout = finding("timeout", 5, 2, {
    title: "Timeout default reads 0 when unset",
    body: "`timeoutMs` falls back to 0 when the caller omits it, which disables the timeout.",
    file: "src/auth/config.ts",
    line: 18,
    status: "rejected",
    resolution: "The caller always passes a timeout; 0 is never read.",
    resolvedByRunId: circlesRunId(6),
    resolvedAt: ago(3 * HOUR),
  });
  const redirectAgain = finding("redirect-again", 7, 0, {
    title: "Redirect target is not validated",
    body: "The allow-list check runs after the redirect is built, so the unvalidated target is still the one that is sent when the list is empty.",
  });
  const race = finding("race", 7, 1, {
    title: "Sign-out races the token refresh",
    body: "A refresh in flight can write a new token after sign-out has cleared the old one, signing the user back in.",
    file: "src/auth/session.ts",
    line: 112,
    fingerprint: "src/auth/session.ts|race",
  });
  const nit = finding("nit", 7, 2, {
    title: "Rename redirectTo to returnTo",
    body: "The rest of the module says `returnTo`.",
    severity: "low",
    blocking: false,
    fingerprint: "src/auth/redirect.ts|nit",
  });
  const carried = finding("timeout-again", 7, 3, {
    title: "Timeout default reads 0 when unset",
    body: "`timeoutMs` falls back to 0 when the caller omits it, which disables the timeout.",
    file: "src/auth/config.ts",
    line: 18,
    status: "rejected",
    resolution: "Rejected earlier as rl-circles-timeout: The caller always passes a timeout; 0 is never read.",
    carriedOver: true,
    resolvedAt: ago(2 * HOUR),
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
              finding("migration", 2, 0, {
                title: "Missing migration for the sessions table",
                body: "The new column is read before anything creates it.",
                file: "src-tauri/migrations/sessions.sql",
                line: 3,
                status: "fixed",
                resolution: "Added the migration.",
                resolvedByRunId: circlesRunId(3),
                resolvedAt: ago(DAY),
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
            findings: [redirect, cookie, timeout],
            fix: { phase: phase("fix", [6]), resolved: [redirect, cookie, timeout] },
            regressed: [],
            newAfterFix: [],
            pingPong: false,
          },
          {
            review: phase("review", [7]),
            findings: [redirectAgain, race, nit, carried],
            fix: null,
            regressed: [redirectAgain],
            newAfterFix: [race],
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

function reviewLoopScenario(name: ScenarioName): Scenario {
  positionCounter = 0;
  runCounter = 0;

  const cards = LOOP_CARDS.map((card) => {
    const base = task({
      id: card.id,
      title: card.title,
      column: card.moving ? "ready" : "in_review",
      runState: card.moving ?? "idle",
      lastRun: card.last,
    });
    return {
      ...base,
      branch: `rimaia/${card.id}`,
      reviewLoop: card.loop,
      updatedAt: ago(2 * HOUR),
    };
  });
  const surroundings = [
    task({ id: "t-ready-01", title: IDLE_READY_TITLES[10] }),
    task({ id: "t-ready-02", title: "Add the unattended-runs opt-in to the welcome checklist" }),
    task({
      id: "t-done-1",
      title: "Record the first unattended run's findings",
      column: "done",
      lastRun: lastRun("succeeded", "success", 2 * DAY),
    }),
  ];
  const tasks = [...cards, ...surroundings];
  const byId = new Map(tasks.map((candidate) => [candidate.id, candidate]));

  const runs: RunListEntry[] = [];
  const reviews: Record<string, RunReview> = {};

  // The panel's task: all seven rows, newest first.
  const circles = byId.get(CIRCLES) as TaskSummary;
  for (const [attempt, kind] of [...CIRCLES_ATTEMPTS].reverse()) {
    const id = circlesRunId(attempt);
    const run = runFor(circles, "rimaia-app", (30 - attempt * 3) * HOUR / 4, {
      id,
      attempt,
      kind,
      sessionId: kind === "fix" ? "session-implementation-4" : `session-${attempt}`,
      prompt: `${kind === "implementation" ? "Implement" : kind === "review" ? "Review" : "Fix"}: ${circles.title}`,
      costUsd: kind === "implementation" ? 2.48 : kind === "review" ? 0.62 : 0.91,
      numTurns: kind === "implementation" ? 38 : 14,
      prUrl: `https://github.com/example/rimaia-app/pull/${210}`,
      status: "succeeded",
      exitClass: "success",
    });
    const bundle = smallBundle(run.endedAt ?? ago(HOUR), "Validate the sign-out redirect");
    run.headSha = bundle.commits[0].sha;
    run.baseSha = "c0ffee00c0ffee00c0ffee00c0ffee00c0ffee00";
    reviews[id] = { source: "recorded", bundle };
    runs.push(run);
  }

  // One latest row per other card, agreeing with its summary.
  let index = 0;
  for (const card of LOOP_CARDS) {
    if (card.id === CIRCLES) continue;
    index += 1;
    const summary = byId.get(card.id) as TaskSummary;
    const run = runFor(summary, "rimaia-app", (2 + index) * HOUR + 20 * MINUTE, {
      attempt: card.loop.fixesSpent + card.loop.reviews + 1,
      kind: card.last.kind,
      costUsd: card.last.kind === "implementation" ? 2.48 : 0.62,
      prUrl: null,
    });
    runs.push(run);
    if (run.status === "succeeded") {
      const bundle = smallBundle(run.endedAt ?? ago(HOUR), "Keep each repository's cards together");
      run.headSha = bundle.commits[0].sha;
      reviews[run.id] = { source: "recorded", bundle };
    }
  }
  runs.sort((a, b) => Date.parse(b.startedAt) - Date.parse(a.startedAt));

  const reviewing = byId.get("rl-reviewing") as TaskSummary;
  const reviewingRun = runs.find((run) => run.taskId === reviewing.id) as RunListEntry;
  const tails = [{ ...tailFor(reviewingRun, 1), currentTool: { id: "tool-9", name: "Read", detail: "src/styles/board.css" } }];

  const globalConfig: ReviewConfig = name === "review-loop-settings" ? {} : REVIEW_LOOP_ON;
  const reviewDefaults = { ...BUILT_IN_REVIEW, ...globalConfig };
  const repositoryApp = reviewLevel({ max_review_loops: 1 }, reviewDefaults);
  const repositorySite = reviewLevel({}, reviewDefaults);
  const circlesInherited = repositoryApp.effective;

  const withLoops = analytics(true);
  const reviewLoopSpendUsd = 6.84;
  const reviewAnalytics: Analytics = {
    ...withLoops,
    spendUsd: withLoops.spendUsd + reviewLoopSpendUsd,
    spendByDay: withLoops.spendByDay.map((day, position) =>
      position === 3 ? { ...day, spendUsd: day.spendUsd + reviewLoopSpendUsd, runs: day.runs + 11 } : day,
    ),
    reviewLoopSpendUsd,
    reviewLoopOutcomes: { succeeded: 11, failed: 1, cancelled: 0, interrupted: 0, running: 1 },
  };

  return {
    name,
    appInfo: appInfo(true),
    repositories: repositories(),
    checkouts: checkouts(),
    worktrees: cards.map((card) => worktreeOf(card.id)),
    tasks,
    runs,
    tails,
    reviews,
    ...noReviewExtras(),
    reviewHistories: { [CIRCLES]: circlesHistory() },
    reviewSettings: {
      instructions:
        name === "review-loop-settings" || name === "review-loop-repository"
          ? "Run /review and report anything that would block a merge. Ignore formatting."
          : "",
      config: globalConfig,
    },
    reviewLevels: {
      global: reviewLevel(globalConfig),
      repositories: { [REPO_APP]: repositoryApp, [REPO_SITE]: repositorySite },
      tasks: { [CIRCLES]: reviewLevel({ max_review_loops: 3 }, circlesInherited) },
    },
    taskReviews: {
      [CIRCLES]: {
        instructions: "Run /review, then check the redirect with /security-review.",
        config: { max_review_loops: 3 },
      },
    },
    queueStatus: queueFor(tasks, "running", [reviewing]),
    capacity: { mode: "sequential", maxConcurrency: 1, ceiling: 8 },
    doctor: healthyDoctor(),
    analytics: reviewAnalytics,
    runsReadError: null,
  };
}

function appInfo(onboardingDismissed: boolean): AppInfo {
  return {
    appVersion: "0.1.0",
    dataDir: "/Users/dev/Library/Application Support/Rimaia",
    dbFile: "/Users/dev/Library/Application Support/Rimaia/rimaia.db",
    logsDir: "/Users/dev/Library/Application Support/Rimaia/logs",
    onboardingDismissed,
  };
}

function empty(name: ScenarioName, onboardingDismissed: boolean, withRepository: boolean): Scenario {
  return {
    name,
    appInfo: appInfo(onboardingDismissed),
    repositories: withRepository ? repositories().slice(0, 1) : [],
    checkouts: withRepository ? checkouts().slice(0, 1) : [],
    worktrees: [],
    tasks: [],
    runs: [],
    tails: [],
    reviews: {},
    ...noReviewExtras(),
    queueStatus: queueFor([], "paused", []),
    capacity: { mode: "sequential", maxConcurrency: 1, ceiling: 8 },
    doctor: healthyDoctor(),
    analytics: analytics(false),
    runsReadError: null,
  };
}

export function buildScenario(name: ScenarioName): Scenario {
  switch (name) {
    case "review-digest":
      return reviewDigestScenario();
    case "review-truncated":
      return reviewBoard(name, [
        {
          id: "t-review-1",
          title: "Add the doctor banner to every view",
          review: { source: "recorded", bundle: truncatedBundle(ago(3 * HOUR)) },
        },
        ...QUEUE_TAIL,
      ]);
    case "review-pruned":
      return reviewBoard(name, [
        {
          id: "t-review-1",
          title: "Keep each repository's cards together in the board order",
          review: { source: "recorded", bundle: prunedBundle(ago(3 * HOUR)) },
        },
        ...QUEUE_TAIL,
      ]);
    case "review-no-commits":
      return reviewBoard(name, [
        {
          id: "t-review-1",
          title: "Check the welcome screen against the new doctor wording",
          review: { source: "recorded", bundle: null },
        },
        ...QUEUE_TAIL.slice(0, 1),
      ]);
    case "review-not-recorded":
      return reviewBoard(
        name,
        [
          {
            id: "t-review-1",
            title: "Persist the board's repository filter across restarts",
            review: { source: "not_recorded" },
          },
          ...QUEUE_TAIL,
        ],
        {
          // What the branch holds now. The morning review never asks for it,
          // and the screenshot is the proof: none of these names appear.
          liveDiffs: {
            "t-review-1": {
              taskId: "t-review-1",
              branch: "rimaia/t-review-1",
              baseRef: "main",
              diff: { filesChanged: 1, insertions: 30, deletions: 2 },
              files: [{ path: "src/live-branch-only.ts", insertions: 30, deletions: 2 }],
              commits: [],
            },
          },
        },
      );
    case "review-chain":
      return reviewChain();
    case "review-empty":
      return reviewBoard(name, []);
    case "review-loop":
    case "review-loop-panel":
    case "review-loop-overlay":
    case "review-loop-runs":
    case "review-loop-analytics":
    case "review-loop-settings":
    case "review-loop-repository":
      return reviewLoopScenario(name);
    case "busy":
      return populated(name, 3, true);
    case "one-run":
      return populated(name, 1, false);
    case "two-runs":
      return populated(name, 2, false);
    case "empty":
      return empty(name, true, true);
    case "welcome":
      return { ...empty(name, false, false), doctor: doctor() };
    case "error":
      return {
        ...populated(name, 1, false),
        runsReadError: {
          code: "database",
          message: "could not read tasks: database is locked (another Rimaia window may hold it)",
        },
      };
  }
}

export function isScenarioName(value: string): value is ScenarioName {
  return (SCENARIO_NAMES as readonly string[]).includes(value);
}
