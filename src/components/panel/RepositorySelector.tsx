import { useState } from "react";
import type { ChangeEvent } from "react";

import { toRimaiaError, updateTask } from "../../lib/commands";
import type { Repository, RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface RepositorySelectorProps {
  readonly taskId: string;
  readonly repositoryId: string;
  /**
   * What this task may be re-filed under — the board's own `useRepositories`
   * list, handed down rather than fetched again here. It is already live on
   * `repositories:changed`, and a second `list_repositories` per panel open
   * would buy nothing. Empty is a legitimate state (the read has not landed,
   * or it failed), and means "offer only where the task already is".
   */
  readonly repositories: readonly Repository[];
  /** What to show for `repositoryId` when the list above does not hold it —
   *  the board already resolved this name for the card. */
  readonly repositoryName: string;
  readonly worktreePath: string | null;
  readonly hasRuns: boolean;
  /** True while `get_task` is still in flight: whether the task has runs is
   *  not yet known, and guessing "no" would offer a control this task may
   *  not be entitled to. */
  readonly detailLoading: boolean;
}

/**
 * Task 005's "repository selector", decided by seam-contract D13: a task's
 * repository is reassignable **only while it has no worktree and no runs**.
 *
 * The rule itself is `tasks::update_task`'s, not this component's — it
 * refuses either way, and task 010 will call the same service over MCP where
 * no UI exists to disable (ADR-0006). Disabling the control here is a
 * courtesy that saves a round trip and explains itself; surfacing the
 * service's own refusal underneath it is what makes the rule visible when the
 * courtesy is wrong (a run started in another window between this panel's
 * `get_task` and this change event, say).
 */
export function RepositorySelector({
  taskId,
  repositoryId,
  repositories,
  repositoryName,
  worktreePath,
  hasRuns,
  detailLoading,
}: RepositorySelectorProps) {
  const [selected, setSelected] = useState(repositoryId);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  const blockedReason = reassignmentBlockedReason(worktreePath, hasRuns);

  function handleChange(event: ChangeEvent<HTMLSelectElement>) {
    const next = event.target.value;
    if (next === selected) return;
    const previous = selected;
    // Committed optimistically and reverted on rejection, the same trade
    // `ModelEffortOverrides` makes: nothing updates `repositoryId` until the
    // mutation's `tasks:changed` round-trips back through the board.
    setSelected(next);
    setSaving(true);
    setError(null);
    updateTask(taskId, { repositoryId: next }).then(
      () => setSaving(false),
      (thrown) => {
        setSelected(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  const known = repositories.some((repository) => repository.id === repositoryId);

  return (
    <>
      <select
        className="repository-select"
        // "Task repository", not "Repository": the board toolbar's filter is
        // a `<select>` labelled "Repository" in the same view, and two
        // controls that announce identically while doing different things is
        // the ambiguity the label exists to prevent. The visible `<dt>` beside
        // it still reads "Repository", which this name contains (WCAG 2.5.3),
        // and it matches the `aria-label="Task title"` two rows above.
        aria-label="Task repository"
        value={selected}
        onChange={handleChange}
        disabled={blockedReason !== null || detailLoading || saving}
      >
        {/* The task's own repository is always an option, even before the
            list has arrived or when reading it failed (the board renders
            that error once, at the top) — a selector that cannot show where
            the task is filed is worse than no selector. */}
        {!known && <option value={repositoryId}>{repositoryName}</option>}
        {repositories.map((repository) => (
          <option key={repository.id} value={repository.id}>
            {repository.name}
          </option>
        ))}
      </select>
      {saving && <span className="repository-saving muted">Saving…</span>}
      {blockedReason && <p className="repository-locked muted">{blockedReason}</p>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </>
  );
}

/**
 * Why the selector is disabled, or `null` when it is not — the same two
 * conditions, checked in the same order (worktree first, because it names a
 * place on disk the user can go and look at), that
 * `tasks::ensure_repository_is_reassignable` refuses on.
 *
 * The wording is the service's own sentence with two elisions, both forced.
 * The title is dropped because it is being edited two lines above this text.
 * The run *count* is dropped because the frontend does not have one: task
 * 004 shipped `TaskDetail` with the last run, not a tally (seam-contract
 * D12's summary has none either), and inventing a number would be worse than
 * omitting it. The counted sentence is still what the user sees if the
 * service ever refuses for real — that message is rendered verbatim.
 */
function reassignmentBlockedReason(worktreePath: string | null, hasRuns: boolean): string | null {
  if (worktreePath) {
    return `Cannot move to another repository: it already has a worktree at ${worktreePath}.`;
  }
  if (hasRuns) {
    return "Cannot move to another repository: a run has already been recorded against it.";
  }
  return null;
}
