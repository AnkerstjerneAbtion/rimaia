import { useMemo } from "react";

import { listLocalWorktrees } from "../lib/commands";
import { subscribeToTasksChanged } from "../lib/events";
import type { RimaiaError } from "../types";
import { createSharedRead } from "./sharedRead";

export interface UseLocalWorktreesResult {
  /** Where each task's worktree is on this computer, keyed by task id. A task
   *  with no entry has no worktree here (task 066). */
  readonly worktrees: ReadonlyMap<string, string>;
  readonly loading: boolean;
  readonly error: RimaiaError | null;
  /** Reads again now, after this component's own write. */
  readonly reload: () => void;
}

const useWorktreeList = createSharedRead(listLocalWorktrees, subscribeToTasksChanged);

/**
 * This computer's worktree records (task 066): what task DTOs carried as
 * `worktreePath` until no board DTO held a path. Components join it to a task
 * by id.
 *
 * Re-read on `tasks:changed`, which every worktree-record write publishes
 * until task 048 turns it into `LocalChange::Worktrees` on the same wire name.
 * Every payload means "re-read", including the lagged forwarder's empty array
 * (seam-contract D7). One read is shared by every component mounted at once.
 */
export function useLocalWorktrees(): UseLocalWorktreesResult {
  const { value, error, reload } = useWorktreeList();
  const worktrees = useMemo(
    () => new Map((value ?? []).map((entry) => [entry.taskId, entry.path])),
    [value],
  );
  return { worktrees, loading: value === null && error === null, error, reload };
}
