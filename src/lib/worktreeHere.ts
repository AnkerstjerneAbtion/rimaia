import type { ClientCapabilities, WorktreeRunner } from "../types";

/**
 * Whether the worktree of a task is on the machine this client acts on: the
 * one rule the panel's worktree section and the review view share (task 050
 * Scope 5, ADR-0034 point 5).
 *
 * Solo has one machine, so it is always here. Otherwise it is here only when
 * the capabilities name a local runner and that runner is the one that last
 * worked in the task's worktree. A browser has no local runner, so it is never
 * here; a connected desktop is here for its own runner's tasks and not for
 * another machine's.
 */
export function worktreeIsHere(
  task: { readonly worktreeRunner: WorktreeRunner | null },
  capabilities: ClientCapabilities,
): boolean {
  if (capabilities.mode === "solo") return true;
  return (
    capabilities.localRunnerId !== null &&
    task.worktreeRunner !== null &&
    task.worktreeRunner.id === capabilities.localRunnerId
  );
}
