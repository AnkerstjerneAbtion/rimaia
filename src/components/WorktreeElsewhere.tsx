import type { WorktreeRunner } from "../types";

/**
 * Whose machine holds a task's worktree, when it is not the one this client
 * acts on (task 050 Scope 5, ADR-0034 point 5): the name, in place of an Open
 * worktree it could not honour.
 *
 * One component, so the task panel and the review view cannot word it
 * differently. A task no run has touched has no worktree to name, and renders
 * nothing.
 */
export function WorktreeElsewhere({ runner }: { readonly runner: WorktreeRunner | null }) {
  if (runner === null) return null;
  return (
    <p className="worktree-elsewhere">
      {`Worktree on ${runner.label}${runner.unpaired ? " (unpaired)" : ""}`}
    </p>
  );
}
