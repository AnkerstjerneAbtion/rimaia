import { invoke } from "@tauri-apps/api/core";

import type {
  AppInfo,
  BoardColumn,
  NewTaskInput,
  NewTaskLinkInput,
  RegisterRepositoryInput,
  RemoteInfo,
  Repository,
  RimaiaError,
  RunState,
  Task,
  TaskDetail,
  TaskFilterInput,
  TaskLink,
  TaskLinkPatchInput,
  TaskPatchInput,
  UpdateRepositoryInput,
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

/** `filter` narrows the result; an empty object matches every task. */
export function listTasks(filter: TaskFilterInput = {}): Promise<Task[]> {
  return call<Task[]>("list_tasks", { filter });
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
