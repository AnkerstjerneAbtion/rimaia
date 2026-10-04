import type {
  ArchiveReport,
  CleanupReport,
  CredentialStatus,
  DiffSummary,
  ErrorCode,
  McpProbe,
  McpStatus,
  PlanPass,
  PreflightSummary,
  PruneResult,
  ReviewOutcome,
  RimaiaError,
  Run,
  RunDetail,
  RunListEntry,
  Schedule,
  ScheduleView,
  StrategyCatalogueView,
  Task,
  TaskDependent,
  TaskDetail,
  TaskFilterInput,
  TaskSummary,
  TranscriptPage,
  TranscriptSummary,
  WorktreeInventory,
  WorktreeStatus,
} from "../../types";
import type { Scenario } from "./seed";

/**
 * One answer per command `src/lib/commands.ts` sends, keyed by wire name.
 *
 * **A fixture is a picture, not a backend.** Reads answer from the scenario;
 * writes answer with something plausible and change nothing — the screenshot
 * script never clicks a mutating control, and a stateful fake would be a
 * second backend to keep true. A command with nothing to show is a
 * {@link refuse}, with the reason, rather than a silent `undefined`.
 *
 * Refusals use a code `ErrorCode` already has (seam-contract D8): a fixture
 * that invented one would render a banner no backend can produce.
 */

export type Args = Record<string, unknown>;
export type Answer = (args: Args, scenario: Scenario) => unknown;

/** A thrown refusal, in the shape `toRimaiaError` passes through unchanged. */
export function refuse(code: ErrorCode, message: string): never {
  const error: RimaiaError = { code, message };
  throw error;
}

const unsupported = (reason: string): Answer => () => refuse("internal", reason);

function findTask(scenario: Scenario, id: unknown): TaskSummary {
  const found = scenario.tasks.find((candidate) => candidate.id === id);
  if (!found) refuse("not_found", `fixture mode has no task \`${String(id)}\``);
  return found;
}

/** The scenario's own answer where it has one (the review scenarios do), else
 *  the seeded tasks a card names as blocked by `task`. Beyond those the seed
 *  carries no edges, only each card's `blockingTitle`, and no run there records
 *  a commit, so nothing can have built on anything. */
function dependentsOf(scenario: Scenario, id: unknown): TaskDependent[] {
  const task = findTask(scenario, id);
  const seeded = scenario.dependents[task.id];
  if (seeded) return seeded;
  return scenario.tasks
    .filter((candidate) => candidate.blockingTitle === task.title)
    .map((candidate) => ({
      id: candidate.id,
      title: candidate.title,
      column: candidate.column,
      runState: candidate.runState,
      archivedAt: candidate.archivedAt,
      builtOn: false,
    }));
}

const CHANGES_REQUESTED_BLOCK =
  "Review note (changes requested; the reviewed commits were kept, so build on them):\nFixture note.";
const REJECTED_BLOCK =
  "Review note (rejected; the task restarted on a fresh branch without those commits):\nFixture note.";

function sentBack(
  scenario: Scenario,
  id: unknown,
  block: string,
  rejected: boolean,
): ReviewOutcome {
  const seeded = findTask(scenario, id);
  const extraInstructions = seeded.extraInstructions
    ? `${seeded.extraInstructions}\n\n${block}`
    : block;
  return {
    task: {
      ...seeded,
      column: "ready",
      extraInstructions,
      ...(rejected ? { branch: null, worktreePath: null } : {}),
    },
    dependents: dependentsOf(scenario, id),
    setAsideBranch: rejected ? seeded.branch : null,
  };
}

function latestRun(scenario: Scenario, taskId: string): RunListEntry | null {
  return scenario.runs.find((run) => run.taskId === taskId) ?? null;
}

function plainRun(entry: RunListEntry): Run {
  const { taskTitle, repositoryId, repositoryName, logAvailable, ...run } = entry;
  void taskTitle, repositoryId, repositoryName, logAvailable;
  return run;
}

function detailOf(scenario: Scenario, id: unknown): TaskDetail {
  const summary = findTask(scenario, id);
  const run = latestRun(scenario, summary.id);
  return {
    ...summary,
    links: [],
    dependsOn: scenario.dependencies[summary.id] ?? [],
    lastRun: run ? plainRun(run) : null,
  };
}

function matchesFilter(candidate: TaskSummary, filter: TaskFilterInput): boolean {
  if (filter.repositoryId && candidate.repositoryId !== filter.repositoryId) return false;
  if (filter.column && candidate.column !== filter.column) return false;
  if (filter.runState && candidate.runState !== filter.runState) return false;
  const archived = filter.archived ?? "active";
  if (archived === "active" && candidate.archivedAt !== null) return false;
  if (archived === "archived" && candidate.archivedAt === null) return false;
  return true;
}


const CATALOGUE: StrategyCatalogueView = {
  catalogue: {
    models: [
      { id: "opus", label: "Opus" },
      { id: "sonnet", label: "Sonnet" },
    ],
    efforts: [
      { id: "medium", label: "Medium" },
      { id: "high", label: "High" },
    ],
    planner: { model: "opus", effort: "high", max_turns: 20 },
  },
  json: "{}",
  defaultJson: "{}",
  providerInfo: { id: "claude_code", displayName: "Claude Code" },
};

const NO_CREDENTIAL: CredentialStatus = {
  configured: false,
  login: null,
  label: null,
  addedAt: null,
  store: { state: "absent" },
  sshRemote: false,
};

const MCP_STATUS: McpStatus = {
  state: "listening",
  configuredPort: 47821,
  boundAddress: "127.0.0.1:47821",
  message: null,
};

const MCP_PROBE: McpProbe = {
  endpoint: "http://127.0.0.1:47821/mcp",
  latencyMs: 4,
  serverName: "rimaia",
  protocolVersion: "2025-06-18",
  toolCount: 14,
};

const EMPTY_CLEANUP: CleanupReport = { removed: [], refused: [], bytesFreed: 0 };

const EMPTY_ARCHIVE: ArchiveReport = { archived: [], refused: [] };

const EMPTY_TRANSCRIPT: TranscriptPage = { entries: [], offset: 0, totalLines: 0 };

const TRANSCRIPT_SUMMARY: TranscriptSummary = {
  permissionMode: "bypassPermissions",
  model: "claude-opus-4-1",
  deniedToolCalls: 0,
  endedWithResult: true,
  endsMidLine: false,
  malformedLines: 0,
};

const EMPTY_PLAN_PASS: PlanPass = { results: [], planned: 0, skipped: 0, spentUsd: 0, cancelled: false };

const EMPTY_PRUNE: PruneResult = { runsPruned: 0, strategyTranscriptsPruned: 0, bytesFreed: 0 };

const NO_DIFF = { filesChanged: 0, insertions: 0, deletions: 0 };

const SCHEDULE: Schedule = {
  id: "schedule-nightly",
  name: "Nightly",
  mode: "parallel",
  cron: "0 22 * * *",
  startAt: null,
  maxConcurrency: 3,
  enabled: true,
  timezone: "UTC",
  stopAt: null,
  lastFiredAt: null,
  armedAt: null,
};

const SCHEDULE_VIEW: ScheduleView = {
  ...SCHEDULE,
  nextFireAt: "2026-10-04T22:00:00.000Z",
  nextFireError: null,
};

const done: Answer = () => undefined;

export const ANSWERS: Record<string, Answer> = {
  // --- app --------------------------------------------------------------
  get_app_info: (_args, s) => s.appInfo,
  reveal_app_data_dir: done,
  debug_provoke_error: unsupported("debug_provoke_error has nothing to show"),

  // --- repositories -----------------------------------------------------
  list_repositories: (_args, s) => s.repositories,
  register_repository: unsupported("registering a repository needs a real filesystem"),
  update_repository: (args, s) => s.repositories.find((r) => r.id === args.id) ?? s.repositories[0],
  set_repository_unattended_runs: (args, s) =>
    s.repositories.find((r) => r.id === args.id) ?? s.repositories[0],
  set_repository_on_archive: (args, s) =>
    s.repositories.find((r) => r.id === args.id) ?? s.repositories[0],
  set_repository_max_concurrency: (args, s) =>
    s.repositories.find((r) => r.id === args.id) ?? s.repositories[0],
  remove_repository: done,
  get_repository_remote_info: () => ({
    remoteUrl: "git@github.com:example/rimaia-app.git",
    ghReady: true,
  }),
  get_repository_credential_status: () => NO_CREDENTIAL,
  set_repository_credential: unsupported("credentials are never stored in fixture mode"),
  remove_repository_credential: () => NO_CREDENTIAL,

  // --- tasks ------------------------------------------------------------
  create_task: unsupported("fixture mode does not create tasks"),
  get_task: (args, s) => detailOf(s, args.id),
  list_tasks: (args, s) => {
    const filter = (args.filter ?? {}) as TaskFilterInput;
    if (s.runsReadError && filter.runState === "running") throw s.runsReadError;
    return s.tasks.filter((candidate) => matchesFilter(candidate, filter));
  },
  update_task: (args, s) => findTask(s, args.id),
  delete_task: done,
  archive_task: unsupported("fixture mode does not archive tasks"),
  archive_tasks: () => EMPTY_ARCHIVE,
  unarchive_task: (args, s) => findTask(s, args.id),
  move_task: (args, s) => findTask(s, args.id),
  approve_task: (args, s): Task => ({ ...findTask(s, args.taskId), column: "done" }),
  reject_task: (args, s) => sentBack(s, args.taskId, REJECTED_BLOCK, true),
  request_task_changes: (args, s) => sentBack(s, args.taskId, CHANGES_REQUESTED_BLOCK, false),
  get_task_dependents: (args, s) => dependentsOf(s, args.taskId),
  get_review_digest: (_args, s) => s.digest,
  mark_review_digest_seen: (args) => args.through,
  set_task_run_state: (args, s) => findTask(s, args.id),
  add_task_link: unsupported("fixture mode does not edit links"),
  update_task_link: unsupported("fixture mode does not edit links"),
  remove_task_link: done,
  reorder_task_link: unsupported("fixture mode does not edit links"),
  set_task_dependencies: (args) => (Array.isArray(args.dependsOn) ? args.dependsOn : []),
  get_blocking_reason: (args, s): Task[] => {
    const blocked = s.tasks.find((candidate) => candidate.id === args.taskId);
    return blocked?.blockingTitle ? [blocked] : [];
  },

  // --- settings ---------------------------------------------------------
  get_base_instructions: () =>
    "Work in the worktree you were given. Keep the change small, run the project's checks, and describe what you could not verify.",
  set_base_instructions: done,
  get_run_environment: () => "inherit",
  get_run_cost_summary: () => ({
    medianUsd: 1.9,
    sampleSize: 12,
    inheritCostUsd: 2.4,
    providerDisplayName: "Claude Code",
  }),
  set_run_environment: done,
  preview_composed_prompt: (args, s) => `Implement: ${findTask(s, args.taskId).title}`,

  // --- strategy ---------------------------------------------------------
  get_strategy_catalogue: () => CATALOGUE,
  set_strategy_catalogue: () => CATALOGUE,
  get_strategy_defaults: () => ({ mode: "default" }),
  set_strategy_defaults: done,
  get_strategy_approval: () => "automatic",
  set_strategy_approval: done,
  accept_task_strategy: (args, s) => findTask(s, args.taskId),
  clear_task_strategy: (args, s) => findTask(s, args.taskId),
  plan_task_strategy: done,
  plan_tasks_strategy: () => EMPTY_PLAN_PASS,
  cancel_plan_pass: done,

  // --- worktrees --------------------------------------------------------
  get_worktree_status: (args): WorktreeStatus => ({
    taskId: String(args.taskId),
    exists: false,
    path: null,
    branch: null,
    baseRef: "main",
    dependencyWarning: null,
    ahead: 0,
    behind: 0,
    dirty: false,
    commitCount: 0,
    diff: NO_DIFF,
  }),
  get_diff_summary: (args, s): DiffSummary =>
    s.liveDiffs[String(args.taskId)] ?? {
      taskId: String(args.taskId),
      branch: null,
      baseRef: "main",
      diff: NO_DIFF,
      files: [],
      commits: [],
    },
  reveal_task_worktree: done,
  list_open_in_targets: () => [
    { target: "vs_code", label: "VS Code" },
    { target: "terminal", label: "Terminal" },
  ],
  open_task_worktree_in: done,
  get_worktree_inventory: (): WorktreeInventory => ({ entries: [], totalBytes: 0 }),
  remove_task_worktree: unsupported("fixture mode has no worktrees to remove"),
  cleanup_done_worktrees: () => EMPTY_CLEANUP,
  cleanup_merged_worktrees: () => EMPTY_CLEANUP,
  get_worktree_auto_cleanup: () => "off",
  set_worktree_auto_cleanup: done,

  // --- runs -------------------------------------------------------------
  start_task_run: done,
  cancel_task_run: done,
  retry_task_now: done,
  give_up_on_task: done,
  get_run_tail: (args, s) => s.tails.find((tail) => tail.runId === args.runId) ?? null,
  list_runs_for_task: (args, s): Run[] =>
    s.runs.filter((run) => run.taskId === args.taskId).map(plainRun),
  list_runs: (_args, s) => s.runs,
  get_run: (args, s): RunDetail => {
    const entry = s.runs.find((run) => run.id === args.runId);
    if (!entry) refuse("not_found", `fixture mode has no run \`${String(args.runId)}\``);
    return {
      ...plainRun(entry),
      // A run the seed recorded nothing for reads as a row from before task
      // 033, and the overlay then asks `get_diff_summary` below.
      review: s.reviews[entry.id] ?? { source: "not_recorded" },
      logAvailable: entry.logAvailable,
    };
  },
  read_run_transcript_page: () => EMPTY_TRANSCRIPT,
  search_run_transcript: () => [],
  summarize_run_transcript: () => TRANSCRIPT_SUMMARY,
  reveal_run_log: done,
  get_run_log_size: (_args, s) => s.runs.length * 412_000,
  prune_run_logs: () => EMPTY_PRUNE,

  // --- schedules --------------------------------------------------------
  list_schedules: (_args, s): ScheduleView[] => (s.tasks.length > 0 ? [SCHEDULE_VIEW] : []),
  create_schedule: unsupported("fixture mode does not create schedules"),
  update_schedule: () => SCHEDULE,
  set_schedule_enabled: () => SCHEDULE,
  delete_schedule: done,
  preview_schedule_preflight: (_args, s): PreflightSummary => ({
    scheduleId: SCHEDULE.id,
    scheduleName: SCHEDULE.name,
    nextFireAt: SCHEDULE_VIEW.nextFireAt,
    closesAt: null,
    mode: SCHEDULE.mode,
    maxConcurrency: SCHEDULE.maxConcurrency,
    plan: s.queueStatus.plan,
  }),
  list_timezones: () => ["UTC", "Europe/Copenhagen", "America/New_York"],

  // --- queue ------------------------------------------------------------
  start_queue: done,
  resume_queue: done,
  pause_queue: done,
  stop_queue: done,
  get_queue_status: (_args, s) => s.queueStatus,
  get_run_capacity: (_args, s) => s.capacity,
  set_schedule_mode: (_args, s) => s.capacity,
  set_max_concurrency: (_args, s) => s.capacity,

  // --- MCP --------------------------------------------------------------
  get_mcp_status: () => MCP_STATUS,
  set_mcp_port: () => MCP_STATUS,
  test_mcp_connection: () => MCP_PROBE,

  // --- analytics --------------------------------------------------------
  get_analytics: (_args, s) => s.analytics,
  get_subscription_cost: (_args, s) => s.analytics.subscriptionMonthlyUsd,
  set_subscription_cost: done,

  // --- doctor and onboarding -------------------------------------------
  run_doctor: (_args, s) => s.doctor,
  dismiss_onboarding: done,
  dismiss_doctor_warning: (_args, s) => s.doctor.dismissals,
  restore_doctor_warning: (_args, s) => s.doctor.dismissals,
};

