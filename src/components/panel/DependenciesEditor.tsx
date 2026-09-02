import { useEffect, useState } from "react";

import { listTasks, setTaskDependencies, toRimaiaError } from "../../lib/commands";
import type { BoardColumn, RimaiaError, TaskSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { COLUMN_TITLES } from "../board/Column";

/**
 * Whether a dependency in this column counts as satisfied — ADR-0008's
 * `in_review` or `done`, and nothing else.
 *
 * A copy of `BoardColumn::satisfies_a_dependency`, and the *only* copy the
 * frontend is allowed: this decides what a row in this list looks like, never
 * whether a task may run. The gate itself is `blocked_by_incomplete`, computed
 * in SQL, so a disagreement here shows a wrong tick and cannot start a task the
 * queue would have held (ADR-0006's rule about which of two doors enforces a
 * rule — this is neither door, it is a label).
 */
function isSatisfied(column: BoardColumn): boolean {
  return column === "in_review" || column === "done";
}

interface DependenciesEditorProps {
  readonly taskId: string;
  /** The repository the task is in — a dependency must be in the same one
   *  (ADR-0008: "a dependency implies a shared branch base"), so it is also the
   *  filter for the picker below. */
  readonly repositoryId: string;
  /** Bare ids, as `get_task` returns them. */
  readonly dependsOn: readonly string[];
  readonly loading: boolean;
  /** Refetches the owning task's detail. `set_task_dependencies` replaces the
   *  whole set server-side and this component holds no copy of the graph to
   *  reconcile against. */
  readonly onChanged: () => void;
}

/**
 * ADR-0008's dependency editor: search and pick tasks in the same repository,
 * with each edge's resolved status shown beside it.
 *
 * `TaskDetail.dependsOn` is bare ids, so this fetches the repository's board to
 * turn them into titles and columns — one `list_tasks` call, which is the same
 * bulk read the board itself uses (seam-contract D12) and is what the picker
 * needs anyway.
 *
 * Every write is a **whole-set replace**, including removal, because that is the
 * only operation the service exposes: cycle detection has to see the complete
 * proposed set to be sound (see `tasks::dependencies`' own header), and an
 * add/remove pair would be two ways to reach one invariant.
 */
export function DependenciesEditor({
  taskId,
  repositoryId,
  dependsOn,
  loading,
  onChanged,
}: DependenciesEditorProps) {
  const [error, setError] = useState<RimaiaError | null>(null);
  const [candidates, setCandidates] = useState<readonly TaskSummary[] | null>(null);
  const [search, setSearch] = useState("");
  const [picked, setPicked] = useState("");
  const [adding, setAdding] = useState(false);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [confirmingId, setConfirmingId] = useState<string | null>(null);

  // Re-read whenever the set changes as well as on mount: a task just added as
  // a dependency has to appear in the list with its title, and a task that
  // moved column elsewhere has to stop reading "waiting".
  useEffect(() => {
    let live = true;
    listTasks({ repositoryId }).then(
      (tasks) => {
        if (live) setCandidates(tasks);
      },
      (thrown) => {
        if (live) setError(toRimaiaError(thrown));
      },
    );
    return () => {
      live = false;
    };
  }, [repositoryId, dependsOn]);

  const byId = new Map((candidates ?? []).map((task) => [task.id, task]));
  const edges = dependsOn.map((id) => ({ id, task: byId.get(id) ?? null }));

  // Everything in this repository that is not this task and not already a
  // dependency. A cycle is *not* filtered out here — the service refuses it by
  // name, and this list guessing at the graph would either hide a legal choice
  // or reimplement the walk in TypeScript.
  const available = (candidates ?? []).filter(
    (task) => task.id !== taskId && !dependsOn.includes(task.id),
  );
  const needle = search.trim().toLowerCase();
  const matches = needle
    ? available.filter((task) => task.title.toLowerCase().includes(needle))
    : available;

  function replace(next: readonly string[], onSettled: () => void) {
    setError(null);
    setTaskDependencies(taskId, [...next]).then(
      () => {
        onSettled();
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        onSettled();
      },
    );
  }

  function handleAdd() {
    if (!picked) return;
    setAdding(true);
    replace([...dependsOn, picked], () => {
      setAdding(false);
      setPicked("");
      setSearch("");
    });
  }

  function handleRemove(id: string) {
    setBusyId(id);
    replace(
      dependsOn.filter((existing) => existing !== id),
      () => {
        setBusyId(null);
        setConfirmingId(null);
      },
    );
  }

  return (
    <section className="task-detail-section">
      <h4>Dependencies</h4>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      {loading && <p className="muted">Loading…</p>}
      {!loading && edges.length === 0 && (
        <p className="muted">
          Nothing yet. A task with dependencies waits for them to reach review, and branches
          from the first one that has.
        </p>
      )}

      {!loading && edges.length > 0 && (
        <ul className="dependency-list">
          {edges.map(({ id, task }) => (
            <li key={id} className="dependency-item">
              <span className="dependency-title">{task ? task.title : id}</span>
              {/* The resolved status per edge, which is the thing task 011's
                  Scope asks this list to show. A dependency the board read did
                  not return (deleted, or in another repository after a
                  hand-edit) is neither satisfied nor waiting — it is unknown,
                  and saying "waiting" would be a guess. */}
              {task ? (
                <span
                  className={
                    isSatisfied(task.column)
                      ? "dependency-status dependency-status-satisfied"
                      : "dependency-status dependency-status-waiting"
                  }
                >
                  {isSatisfied(task.column)
                    ? `Satisfied — ${COLUMN_TITLES[task.column]}`
                    : `Waiting — ${COLUMN_TITLES[task.column]}`}
                </span>
              ) : (
                <span className="dependency-status muted">Not on this board</span>
              )}
              <div className="dependency-item-actions">
                {/* An inline two-state confirm rather than `window.confirm`, the
                    same as `LinksEditor` and `DeleteTaskSection`: a native
                    dialog is untestable in jsdom and blocks the whole app. */}
                {confirmingId === id ? (
                  <>
                    <button
                      type="button"
                      onClick={() => handleRemove(id)}
                      disabled={busyId === id}
                    >
                      {busyId === id ? "Removing…" : "Confirm"}
                    </button>
                    <button
                      type="button"
                      onClick={() => setConfirmingId(null)}
                      disabled={busyId === id}
                    >
                      Cancel
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={() => setConfirmingId(id)}
                    disabled={busyId !== null}
                    aria-label={`Remove the dependency on ${task ? task.title : id}`}
                  >
                    Remove
                  </button>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}

      <div className="dependency-add-form">
        <input
          type="text"
          placeholder="Search this repository's tasks"
          aria-label="Search for a task to depend on"
          value={search}
          onChange={(event) => setSearch(event.target.value)}
        />
        <select
          aria-label="Task to depend on"
          value={picked}
          onChange={(event) => setPicked(event.target.value)}
        >
          <option value="">Choose a task…</option>
          {matches.map((task) => (
            <option key={task.id} value={task.id}>
              {task.title}
            </option>
          ))}
        </select>
        <button type="button" onClick={handleAdd} disabled={adding || picked === ""}>
          {adding ? "Adding…" : "Add dependency"}
        </button>
      </div>
    </section>
  );
}
