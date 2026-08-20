import { useState } from "react";

import { deleteTask, toRimaiaError } from "../../lib/commands";
import type { RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface DeleteTaskSectionProps {
  readonly taskId: string;
  readonly title: string;
  /** Called after a successful delete. `Board`'s own "task missing" effect
   *  would close the panel too, on the next `tasks:changed`, but not before
   *  an extra render pointed at a task that no longer exists. */
  readonly onDeleted: () => void;
}

/** Delete, with confirmation (task 005) — same inline confirm shape as
 *  `RepositoriesSection`'s unattended-runs toggle: the destructive action is
 *  the harder-to-reach second click, never the friendly default. */
export function DeleteTaskSection({ taskId, title, onDeleted }: DeleteTaskSectionProps) {
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  function handleConfirm() {
    setDeleting(true);
    setError(null);
    deleteTask(taskId).then(onDeleted, (thrown) => {
      setError(toRimaiaError(thrown));
      setDeleting(false);
    });
  }

  return (
    <section className="task-detail-section task-detail-delete">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {confirming ? (
        <div
          className="task-detail-delete-confirm"
          role="alertdialog"
          aria-label={`Confirm delete "${title}"`}
        >
          <p>Delete &ldquo;{title}&rdquo;? This can&rsquo;t be undone.</p>
          <div className="task-detail-delete-actions">
            <button type="button" onClick={() => setConfirming(false)} disabled={deleting}>
              Cancel
            </button>
            <button type="button" className="btn-danger" onClick={handleConfirm} disabled={deleting}>
              {deleting ? "Deleting…" : "Delete task"}
            </button>
          </div>
        </div>
      ) : (
        <button type="button" className="btn-danger" onClick={() => setConfirming(true)}>
          Delete task
        </button>
      )}
    </section>
  );
}
