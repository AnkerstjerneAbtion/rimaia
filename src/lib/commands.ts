import { invoke } from "@tauri-apps/api/core";

import type {
  AppInfo,
  BoardColumn,
  DiffSummary,
  DoctorReport,
  McpProbe,
  McpStatus,
  NewTaskInput,
  NewTaskLinkInput,
  PreflightSummary,
  PruneCriterionInput,
  PruneResult,
  QueueStatus,
  RegisterRepositoryInput,
  RemoteInfo,
  Repository,
  RimaiaError,
  Run,
  RunCapacity,
  RunCostSummary,
  RunDetail,
  RunEnvironment,
  RunFilterInput,
  RunListEntry,
  RunState,
  RunTail,
  Schedule,
  ScheduleInput,
  ScheduleMode,
  ScheduleView,
  SearchHit,
  StrategyApproval,
  StrategyCatalogueView,
  StrategyDefaults,
  Task,
  TaskDetail,
  TaskFilterInput,
  TaskLink,
  TaskLinkPatchInput,
  TaskPatchInput,
  TaskSummary,
  TranscriptPage,
  TranscriptSummary,
  UpdateRepositoryInput,
  WorktreeStatus,
} from "../types";

/**
 * The only module in the frontend that imports `invoke`.
 *
 * Every backend call goes through `call`, so the serialization boundary has one
 * place to be wrong instead of one per component — and so a rejected command
 * always arrives as a `RimaiaError` with a readable `message`, never as an
 * object that stringifies to `[object Object]`.
 */
async function call<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (thrown) {
    throw toRimaiaError(thrown);
  }
}

/**
 * Anything can come back across the IPC boundary — the backend's `{code, message}`
 * payload, a plugin's bare string, or a JS exception from `invoke` itself. All
 * three have to end up renderable.
 */
export function toRimaiaError(thrown: unknown): RimaiaError {
  if (isRimaiaError(thrown)) {
    return thrown;
  }
  if (thrown instanceof Error) {
    return { code: "internal", message: thrown.message };
  }
  if (typeof thrown === "string") {
    return { code: "internal", message: thrown };
  }
  return { code: "internal", message: JSON.stringify(thrown) };
}

function isRimaiaError(value: unknown): value is RimaiaError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as RimaiaError).code === "string" &&
    typeof (value as RimaiaError).message === "string"
  );
}

export function getAppInfo(): Promise<AppInfo> {
  return call<AppInfo>("get_app_info");
}

export function revealAppDataDir(): Promise<void> {
  return call<void>("reveal_app_data_dir");
}

/** Only registered in debug builds — see `commands::app::debug_provoke_error`. */
export function debugProvokeError(): Promise<void> {
  return call<void>("debug_provoke_error");
}

// ---------------------------------------------------------------------------
// Repositories (task 003) — see `src-tauri/src/commands/repositories.rs`.
// ---------------------------------------------------------------------------

export function listRepositories(): Promise<Repository[]> {
  return call<Repository[]>("list_repositories");
}

export function registerRepository(input: RegisterRepositoryInput): Promise<Repository> {
  return call<Repository>("register_repository", { input });
}

export function updateRepository(
  id: string,
  patch: UpdateRepositoryInput,
): Promise<Repository> {
  return call<Repository>("update_repository", { id, patch });
}

/** The caller is responsible for the ADR-0012 confirmation dialog before
 *  passing `allow: true` — this only performs the already-agreed act. */
export function setRepositoryUnattendedRuns(id: string, allow: boolean): Promise<Repository> {
  return call<Repository>("set_repository_unattended_runs", { id, allow });
}

/**
 * Raises or lowers ADR-0010's per-repository cap.
 *
 * Its own command rather than a field on {@link updateRepository}, for the
 * reason the unattended-runs opt-in has one: it is a deliberate act with a
 * consequence the panel has to state next to it, not a preference to bury in an
 * "edit name and branch" form. Refused outside `1..=ceiling` — see {@link
 * getRunCapacity} for the ceiling.
 */
export function setRepositoryMaxConcurrency(
  id: string,
  maxConcurrency: number,
): Promise<Repository> {
  return call<Repository>("set_repository_max_concurrency", { id, maxConcurrency });
}

export function removeRepository(id: string): Promise<void> {
  return call<void>("remove_repository", { id });
}

export function getRepositoryRemoteInfo(id: string): Promise<RemoteInfo> {
  return call<RemoteInfo>("get_repository_remote_info", { id });
}

// ---------------------------------------------------------------------------
// Tasks (task 004) — see `src-tauri/src/commands/tasks.rs`.
// ---------------------------------------------------------------------------

export function createTask(input: NewTaskInput): Promise<Task> {
  return call<Task>("create_task", { input });
}

export function getTask(id: string): Promise<TaskDetail> {
  return call<TaskDetail>("get_task", { id });
}

/**
 * The board's bulk read: one query for every card, each carrying its link and
 * dependency counts and a summary of its last run (seam-contract D12). Reach
 * for `getTask` only for the panel — a `get_task` per card is N+1 against the
 * single SQLite writer on every `tasks:changed`.
 *
 * `filter` narrows the result; an empty object matches every task.
 */
export function listTasks(filter: TaskFilterInput = {}): Promise<TaskSummary[]> {
  return call<TaskSummary[]>("list_tasks", { filter });
}

export function updateTask(id: string, patch: TaskPatchInput): Promise<Task> {
  return call<Task>("update_task", { id, patch });
}

export function deleteTask(id: string): Promise<void> {
  return call<void>("delete_task", { id });
}

/**
 * Moves a task to `column`, landing it between `beforeId` and `afterId`.
 * Naming neither is only legal when the destination column is otherwise
 * empty — see `rimaia_core::tasks::move_task`'s own doc for why this is
 * refused rather than guessed as "append" everywhere else.
 */
export function moveTask(
  id: string,
  column: BoardColumn,
  beforeId: string | null,
  afterId: string | null,
): Promise<Task> {
  return call<Task>("move_task", { id, column, beforeId, afterId });
}

export function setTaskRunState(id: string, runState: RunState): Promise<Task> {
  return call<Task>("set_task_run_state", { id, runState });
}

export function addTaskLink(taskId: string, input: NewTaskLinkInput): Promise<TaskLink> {
  return call<TaskLink>("add_task_link", { taskId, input });
}

export function updateTaskLink(
  linkId: string,
  patch: TaskLinkPatchInput,
): Promise<TaskLink> {
  return call<TaskLink>("update_task_link", { linkId, patch });
}

export function removeTaskLink(linkId: string): Promise<void> {
  return call<void>("remove_task_link", { linkId });
}

export function reorderTaskLink(
  linkId: string,
  beforeId: string | null,
  afterId: string | null,
): Promise<TaskLink> {
  return call<TaskLink>("reorder_task_link", { linkId, beforeId, afterId });
}

/**
 * Replaces the whole set of tasks `taskId` is blocked by (ADR-0008).
 *
 * **Replace, never merge** — send the complete set every time, and an empty
 * array clears every dependency. Rejects with the service's own message when
 * the set would close a cycle (naming the whole path), when a task depends on
 * itself, or when the dependency is in another repository. Resolves with the
 * stored set, sorted.
 */
export function setTaskDependencies(taskId: string, dependsOn: string[]): Promise<string[]> {
  return call<string[]>("set_task_dependencies", { taskId, dependsOn });
}

/** The dependencies keeping a task out of the queue, whole rows, in the order
 *  ADR-0008 picks a base branch in. Empty means nothing is blocking it. */
export function getBlockingReason(taskId: string): Promise<Task[]> {
  return call<Task[]>("get_blocking_reason", { taskId });
}

// ---------------------------------------------------------------------------
// Settings (task 006) — see `src-tauri/src/commands/settings.rs`.
// ---------------------------------------------------------------------------

export function getBaseInstructions(): Promise<string> {
  return call<string>("get_base_instructions");
}

export function setBaseInstructions(value: string): Promise<void> {
  return call<void>("set_base_instructions", { value });
}

export function getRunEnvironment(): Promise<RunEnvironment> {
  return call<RunEnvironment>("get_run_environment");
}
/** What runs on this machine have actually cost — see `RunCostSummary`. */
export async function getRunCostSummary(): Promise<RunCostSummary> {
  return call<RunCostSummary>("get_run_cost_summary");
}


export function setRunEnvironment(value: RunEnvironment): Promise<void> {
  return call<void>("set_run_environment", { value });
}

/**
 * The prompt `taskId` would receive right now, byte for byte — calls the
 * same `compose_prompt` a run does (ADR-0009), never a frontend-side
 * approximation. Read fresh on every call; nothing here caches.
 */
export function previewComposedPrompt(taskId: string): Promise<string> {
  return call<string>("preview_composed_prompt", { taskId });
}

// ---------------------------------------------------------------------------
// Execution strategy (task 020) — see `src-tauri/src/commands/strategy.rs`.
//
// The *mode* is deliberately absent from this list: it travels as
// `strategyMode` on {@link updateTask}'s patch, so the panel and the
// `update_task` MCP tool reach the same rule (ADR-0006).
// ---------------------------------------------------------------------------

/**
 * The model and effort lists every strategy dropdown draws from, the stored
 * text Settings' editor opens on, and the bytes "Restore defaults" writes — one
 * read, because they are one settings key.
 *
 * Re-read on {@link subscribeToSettingsChanged} in `./events`: adding a model
 * is a settings write, and a second window has to learn about it.
 */
export function getStrategyCatalogue(): Promise<StrategyCatalogueView> {
  return call<StrategyCatalogueView>("get_strategy_catalogue");
}

/**
 * Stores an edited catalogue, answering with the new view so the caller never
 * re-reads to find out what it stored.
 *
 * Rejects unparseable JSON with the parser's own message, which is what the
 * editor renders inline — an edit that was accepted and then quietly ignored
 * is not something to make a user discover from a log file.
 */
export function setStrategyCatalogue(value: string): Promise<StrategyCatalogueView> {
  return call<StrategyCatalogueView>("set_strategy_catalogue", { value });
}

/**
 * One repository's default strategy, or the global one under it when
 * `repositoryId` is `null`.
 *
 * Never "the effective strategy for a task" — that chain is resolved on the
 * backend and arrives as {@link TaskSummary.effectiveModel} and its siblings.
 */
export function getStrategyDefaults(
  repositoryId: string | null = null,
): Promise<StrategyDefaults> {
  return call<StrategyDefaults>("get_strategy_defaults", { repositoryId });
}

/** Writes one repository's default strategy, or the global one. There is no
 *  "clear": a `"default"` mode with no model and no effort already means "no
 *  opinion". */
export function setStrategyDefaults(
  repositoryId: string | null,
  value: StrategyDefaults,
): Promise<void> {
  return call<void>("set_strategy_defaults", { repositoryId, value });
}

/** Whether a proposal runs on its own or waits for a human. An absent setting
 *  reads `"automatic"`. */
export function getStrategyApproval(): Promise<StrategyApproval> {
  return call<StrategyApproval>("get_strategy_approval");
}

/** Stores the approval setting. **Nothing reads it yet** — the gate lands after
 *  tasks 011 and 012 — but a control that forgot its answer on relaunch would
 *  be worse than none. */
export function setStrategyApproval(value: StrategyApproval): Promise<void> {
  return call<void>("set_strategy_approval", { value });
}

/**
 * Takes authorship of the proposal on `taskId`: `strategySource` flips from
 * `"planner"` to `"user"` (seam-contract D17.7).
 *
 * Accepting a proposal unchanged is this; accepting an *edited* one is
 * {@link updateTask} with the edited model and effort, which flips the same
 * field. There is no `accepted` column behind either.
 */
export function acceptTaskStrategy(taskId: string): Promise<Task> {
  return call<Task>("accept_task_strategy", { taskId });
}

/**
 * Clears the recorded proposal — the panel's "Re-plan", and the only thing that
 * lifts the guard (seam-contract D17.8).
 *
 * A `planned` task with a recorded proposal, successful *or* failed, is not
 * planned again; without it, its next run plans first. Editing the plan text
 * does not re-trigger anything.
 */
export function clearTaskStrategy(taskId: string): Promise<Task> {
  return call<Task>("clear_task_strategy", { taskId });
}

/**
 * Runs the planner for `taskId` now and resolves as soon as it is under way —
 * **not once it finishes**, exactly like {@link startRun}. The proposal arrives
 * on `tasks:changed` when the planner writes it back through Rimaia's own MCP
 * server.
 *
 * Rejects before spawning anything when the repository has not opted in to
 * unattended runs (ADR-0012), when `claude` is missing, or when a run — of
 * either kind — is already in flight for the task.
 */
export function planTaskStrategy(taskId: string): Promise<void> {
  return call<void>("plan_task_strategy", { taskId });
}

// ---------------------------------------------------------------------------
// Worktrees (task 007) — see `src-tauri/src/commands/worktree.rs`.
// ---------------------------------------------------------------------------

export function getWorktreeStatus(taskId: string): Promise<WorktreeStatus> {
  return call<WorktreeStatus>("get_worktree_status", { taskId });
}

/**
 * The diff and the commits a run detail view opens with (task 015,
 * ADR-0013): files changed, insertions, deletions, the per-file breakdown,
 * and the commit list. Scoped to the branch, not to one attempt — every
 * attempt of a task shares one branch, so this is the same summary
 * regardless of which run's detail view fetched it.
 */
export function getDiffSummary(taskId: string): Promise<DiffSummary> {
  return call<DiffSummary>("get_diff_summary", { taskId });
}

/**
 * Opens the task's worktree directory in the OS file manager. "Copy path"
 * needs no command — a component already has the path from
 * {@link getWorktreeStatus} or from `TaskDetail.worktreePath`, and the
 * system clipboard is a browser API away.
 */
export function revealTaskWorktree(taskId: string): Promise<void> {
  return call<void>("reveal_task_worktree", { taskId });
}

// ---------------------------------------------------------------------------
// Runs (task 008) — see `src-tauri/src/commands/runs.rs`.
// ---------------------------------------------------------------------------

/**
 * Starts a manual "Run now" for `taskId` and resolves as soon as it is under
 * way — **not once it finishes.** A long-running run is supervised entirely
 * on the backend; watch `tasks:changed` / `runs:changed` for the row and
 * {@link subscribeToRunsTail} in `./events` for the live view, the same way
 * every other view learns about a change it did not make itself.
 *
 * A repository that has not opted in to unattended runs (ADR-0012), a
 * missing `claude` CLI, or a task already running rejects with a
 * {@link RimaiaError} describing which.
 */
export function startRun(taskId: string): Promise<void> {
  return call<void>("start_task_run", { taskId });
}

/**
 * Asks `taskId`'s in-flight run to stop. A no-op, not an error, when nothing
 * is running for it — the same button can be pressed after the run has
 * already finished.
 */
export function cancelRun(taskId: string): Promise<void> {
  return call<void>("cancel_task_run", { taskId });
}

/**
 * Resumes a task that is waiting out a retry, **now**, without waiting for its
 * deadline (ADR-0011).
 *
 * The operator's override: they can see the window reopened early, or simply
 * want the attempt made while they are watching. It continues the same session
 * rather than starting a fresh one, so the worktree keeps its commits — which
 * is the difference between this and {@link startRun}, and the reason they are
 * two buttons rather than one.
 *
 * Resolves as soon as the run is under way, not once it finishes — same
 * contract as {@link startRun}.
 */
export function retryTaskNow(taskId: string): Promise<void> {
  return call<void>("retry_task_now", { taskId });
}

/**
 * Ends a task's retry loop, landing it in `failed` for a human to look at.
 *
 * For the error that will not clear on its own, where the remaining attempts
 * would each hit the same wall. Rejects for a task that is not waiting — a run
 * in flight is stopped with {@link cancelRun}, and there is nothing to give up
 * on for anything else.
 */
export function giveUpOnTask(taskId: string): Promise<void> {
  return call<void>("give_up_on_task", { taskId });
}

/**
 * The most recent live-tail snapshot the backend has seen for `runId`, or
 * `null` when it has not seen one yet. This is what a client that opens the
 * Runs view after a run has already started reads once to catch up, before
 * subscribing to {@link subscribeToRunsTail} in `./events` for the rest
 * (seam-contract D14) — a run's own outcome and log path are read through
 * {@link getTask}'s `lastRun`, not through this.
 */
export function getRunTail(runId: string): Promise<RunTail | null> {
  return call<RunTail | null>("get_run_tail", { runId });
}

// ---------------------------------------------------------------------------
// Run history and the transcript viewer (task 015, ADR-0013) — see
// `src-tauri/src/commands/runs.rs`.
// ---------------------------------------------------------------------------

/** Every run of `taskId`, newest attempt first — the task detail panel's
 *  history list. */
export function listRunsForTask(taskId: string): Promise<Run[]> {
  return call<Run[]>("list_runs_for_task", { taskId });
}

/**
 * The global Runs view's history: every run matching `filter`, newest first,
 * with its task's title and repository name for a list that spans every
 * repository. `filter` narrows the result; an empty object matches every run.
 */
export function listRuns(filter: RunFilterInput = {}): Promise<RunListEntry[]> {
  return call<RunListEntry[]>("list_runs", { filter });
}

/**
 * One run's full detail: its own outcome, the branch's diff and commits
 * (ADR-0013's ordering), the exact prompt it received, and whether its
 * transcript file still resolves.
 */
export function getRun(runId: string): Promise<RunDetail> {
  return call<RunDetail>("get_run", { runId });
}

/**
 * One page of `runId`'s transcript, oldest-shown-line first. `limit`
 * defaults on the backend to a few hundred lines — pass it explicitly only
 * to change the page size, never to page through a 50MB file in one call.
 */
export function readRunTranscriptPage(
  runId: string,
  offset: number,
  limit?: number,
): Promise<TranscriptPage> {
  return call<TranscriptPage>("read_run_transcript_page", { runId, offset, limit });
}

/**
 * Text search across `runId`'s whole transcript — inside tool inputs as well
 * as assistant messages, since the backend matches the raw JSON line rather
 * than a rendering of it.
 */
export function searchRunTranscript(runId: string, query: string): Promise<SearchHit[]> {
  return call<SearchHit[]>("search_run_transcript", { runId, query });
}

/**
 * How `runId`'s transcript begins and ends — permission mode, model, refused
 * tool calls, and whether the stream reached a `result`. One scan of the file
 * on the backend, so it is read once per run detail rather than carried on
 * every {@link getRun}.
 */
export function summarizeRunTranscript(runId: string): Promise<TranscriptSummary> {
  return call<TranscriptSummary>("summarize_run_transcript", { runId });
}

/** Reveals `runId`'s raw JSONL transcript in the OS file manager. "Copy log
 *  path" needs no command — every caller already has `Run.logPath` from
 *  {@link getRun} or {@link listRunsForTask}, and the system clipboard is a
 *  browser API away. */
export function revealRunLog(runId: string): Promise<void> {
  return call<void>("reveal_run_log", { runId });
}

/** Total bytes on disk across every run's transcript, for Settings' storage
 *  report alongside worktree size. */
export function getRunLogSize(): Promise<number> {
  return call<number>("get_run_log_size");
}

/**
 * Deletes transcript (and stderr) files matching `criterion`, leaving every
 * `runs` row untouched — a pruned run's outcome, diff and commits are still
 * real history, and its log simply reads "unavailable" afterwards, the same
 * state a deleted-by-hand transcript already renders rather than errors on.
 */
export function pruneRunLogs(criterion: PruneCriterionInput): Promise<PruneResult> {
  return call<PruneResult>("prune_run_logs", { criterion });
}

// ---------------------------------------------------------------------------
// Schedules (task 013) — see `src-tauri/src/commands/schedules.rs`.
// ---------------------------------------------------------------------------

/**
 * Every schedule, each with the time it will next fire.
 *
 * The next fire time is the reason this is not a plain table read: task 013's
 * point is that a wrong cron expression is caught **in the evening** rather
 * than discovered in the morning, and a list without it is a list nobody can
 * check. A row whose expression cannot be read reports `nextFireError` instead
 * of a time rather than failing the whole call — the list is where it gets
 * fixed.
 *
 * Re-read on {@link subscribeToSchedulesChanged}.
 */
export function listSchedules(): Promise<ScheduleView[]> {
  return call<ScheduleView[]>("list_schedules");
}

/** Creates a schedule, armed from now — so one created at 23:00 for "every
 *  night at 22:00" means tomorrow, not one minute ago. */
export function createSchedule(input: ScheduleInput): Promise<Schedule> {
  return call<Schedule>("create_schedule", { input });
}

/** Replaces a schedule's configuration, leaving its fire history alone: editing
 *  tonight's stop time does not make tonight's start happen again. */
export function updateSchedule(id: string, input: ScheduleInput): Promise<Schedule> {
  return call<Schedule>("update_schedule", { id, input });
}

/**
 * Turns a schedule on or off without deleting what it is set to.
 *
 * Turning one back on **re-arms** it, so a schedule that spent a month disabled
 * does not immediately fire for the last of thirty nights it missed.
 */
export function setScheduleEnabled(id: string, enabled: boolean): Promise<Schedule> {
  return call<Schedule>("set_schedule_enabled", { id, enabled });
}

export function deleteSchedule(id: string): Promise<void> {
  return call<void>("delete_schedule", { id });
}

/** What this schedule would do if it fired now: which tasks run, in what order,
 *  and which are blocked and why. Computed from the same function the queue
 *  loop itself calls, so it cannot drift from what actually happens. */
export function previewSchedulePreflight(id: string): Promise<PreflightSummary> {
  return call<PreflightSummary>("preview_schedule_preflight", { id });
}

/** Every IANA zone name a schedule may use. The list the picker offers and the
 *  list the service accepts come from one `chrono-tz` table, which is what
 *  keeps a timezone package out of `package.json`. */
export function listTimezones(): Promise<string[]> {
  return call<string[]>("list_timezones");
}

// ---------------------------------------------------------------------------
// The run queue (task 009) — see `src-tauri/src/commands/queue.rs`.
// ---------------------------------------------------------------------------

/** Starts working the `ready` column top-down, one task at a time. Idempotent
 *  — starting an already-running queue is not an error. */
export function startQueue(): Promise<void> {
  return call<void>("start_queue");
}

/** The same action as {@link startQueue}, under the name the user presses
 *  after a pause. */
export function resumeQueue(): Promise<void> {
  return call<void>("resume_queue");
}

/** Starts nothing new; lets the current run finish. */
export function pauseQueue(): Promise<void> {
  return call<void>("pause_queue");
}

/** Pause, plus cancel whatever the queue is currently running. */
export function stopQueue(): Promise<void> {
  return call<void>("stop_queue");
}

/**
 * The whole picture for the Runs view: whether the queue is running, which
 * tasks it holds processes for right now, and every `ready` task in board
 * order with the reason the queue will pass over each one it cannot start.
 * Re-read fresh on every call — subscribe to {@link subscribeToTasksChanged},
 * {@link subscribeToRunsChanged} and {@link subscribeToSettingsChanged} in
 * `./events` and re-fetch on any of them, since a task moving, a run ending,
 * or the queue's own switch (`queue_state` lives in `settings`) can each
 * change what this returns.
 */
export function getQueueStatus(): Promise<QueueStatus> {
  return call<QueueStatus>("get_queue_status");
}

/** How many runs the queue may have in flight, as configured (ADR-0010): the
 *  mode, the stored limit, and the ceiling no setting can raise. One call for
 *  all three, because the control renders them together. */
export function getRunCapacity(): Promise<RunCapacity> {
  return call<RunCapacity>("get_run_capacity");
}

/** Switches the queue between one run at a time and several. Answers with the
 *  whole configuration, so the caller need not re-read what it just wrote. */
export function setScheduleMode(mode: ScheduleMode): Promise<RunCapacity> {
  return call<RunCapacity>("set_schedule_mode", { mode });
}

/** How many runs `"parallel"` may have in flight at once. Refused outside
 *  `1..=ceiling` with a message the panel renders — a stored value out of range
 *  is tolerated and clamped, a value from this form is not. */
export function setMaxConcurrency(value: number): Promise<RunCapacity> {
  return call<RunCapacity>("set_max_concurrency", { value });
}

// ---------------------------------------------------------------------------
// The local MCP server (task 010) — see `src-tauri/src/commands/mcp.rs`.
// ---------------------------------------------------------------------------

/**
 * Whether the MCP server is listening, on which address, and the operating
 * system's own words if its port was taken.
 *
 * A cached snapshot of the bind, which is the thing that fails; it is not a
 * live check that the server is still answering. That is
 * {@link testMcpConnection}. Re-read on mount and on
 * {@link subscribeToSettingsChanged}, since `mcp_port` is a settings key.
 */
export function getMcpStatus(): Promise<McpStatus> {
  return call<McpStatus>("get_mcp_status");
}

/**
 * Stores the port and restarts the server on it, answering with the new
 * status — so a caller never has to re-read to find out what happened.
 *
 * A no-op when the port has not changed: rebinding a socket that is currently
 * listening races itself.
 */
export function setMcpPort(port: number): Promise<McpStatus> {
  return call<McpStatus>("set_mcp_port", { port });
}

/** One real `initialize` + `tools/list` round trip against the running server,
 *  the way a client would — not a "something is listening" check. */
export function testMcpConnection(): Promise<McpProbe> {
  return call<McpProbe>("test_mcp_connection");
}

// ---------------------------------------------------------------------------
// The preflight doctor (task 018) — see `src-tauri/src/commands/doctor.rs`.
// ---------------------------------------------------------------------------

/**
 * Every preflight check, passing rows included.
 *
 * The passing rows are not padding: {@link WelcomeView} shows each of its four
 * steps the rows belonging to it, so "done" there is a check passing rather
 * than a button having been clicked.
 *
 * Spawns up to eight subprocesses, and two more per registered repository, so
 * it is called on mount and on an explicit Re-check — never on an event.
 */
export function runDoctor(): Promise<DoctorReport> {
  return call<DoctorReport>("run_doctor");
}

/**
 * Records that the first-run walkthrough is done with, or deliberately skipped.
 *
 * There is deliberately no command to un-dismiss it: the welcome screen stays
 * reachable from Settings, so a second command would exist only to put back a
 * screen the user can already open.
 */
export function dismissOnboarding(): Promise<void> {
  return call<void>("dismiss_onboarding");
}
