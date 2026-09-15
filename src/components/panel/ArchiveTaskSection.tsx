import { useState } from "react";

import { archiveTask, toRimaiaError, unarchiveTask } from "../../lib/commands";
import { describeCleanup, describeCleanupIntent } from "../../lib/archive";
import type { Repository, RimaiaError, Task } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface ArchiveTaskSectionProps {
  readonly task: Task;
  /** The task's own repository, when the caller has the list. The cleanup
   *  sentence is read off it — a panel with no list can still archive, it just
   *  cannot say in advance what the cleanup will do. */
  readonly repository: Repository | undefined;
  /** Called after a successful archive, the same way `DeleteTaskSection`
   *  closes the panel: `Board`'s "task missing" effect would get there on the
   *  next `tasks:changed`, but not before a render pointed at a card the board
   *  no longer shows. */
  readonly onArchived: () => void;
}

/**
 * ADR-0025's single-card archive, and the way back.
 *
 * Two-click inline confirm, the same shape `DeleteTaskSection` uses — but the
 * sentence is **not** the same, and that is the point of computing it from the
 * repository. "Archive" and "Archive, and delete a 900 MB checkout" are
 * different decisions, so the confirmation names whatever the repository is
 * configured to do before the second click rather than after it.
 *
 * An archived task shows `Unarchive` instead, so the panel a user reaches from
 * the archive list is the same panel, not a read-only copy of it.
 */
export function ArchiveTaskSection({ task, repository, onArchived }: ArchiveTaskSectionProps) {
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [cleanup, setCleanup] = useState<string | null>(null);

  const intent = describeCleanupIntent(repository);

  function fail(thrown: unknown) {
    setError(toRimaiaError(thrown));
    setBusy(false);
  }

  function handleArchive() {
    setBusy(true);
    setError(null);
    archiveTask(task.id).then((archived) => {
      // Shown before the panel closes, because the outcome of a cleanup the
      // user asked for is not something to make them go looking for
      // (ADR-0025 point 6). A clean one says nothing and closes.
      const sentence = describeCleanup(archived.cleanup);
      if (sentence) {
        setCleanup(sentence);
        setBusy(false);
        setConfirming(false);
        return;
      }
      onArchived();
    }, fail);
  }

  function handleUnarchive() {
    setBusy(true);
    setError(null);
    unarchiveTask(task.id).then(() => {
      setBusy(false);
      onArchived();
    }, fail);
  }

  if (task.archivedAt !== null) {
    return (
      <section className="task-detail-section task-detail-archive">
        {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
        <p className="muted">
          Archived. It keeps its runs, its links and everything that depends on it.
        </p>
        <button type="button" onClick={handleUnarchive} disabled={busy}>
          {busy ? "Unarchiving…" : "Unarchive"}
        </button>
      </section>
    );
  }

  return (
    <section className="task-detail-section task-detail-archive">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {cleanup && (
        <p role="status" className="muted">
          {cleanup}
        </p>
      )}
      {confirming ? (
        <div
          className="task-detail-archive-confirm"
          role="alertdialog"
          aria-label={`Confirm archive "${task.title}"`}
        >
          <p>
            Archive &ldquo;{task.title}&rdquo;? It leaves the board and keeps its run history;
            you can put it back.
          </p>
          {intent && <p className="muted">{intent}</p>}
          <div className="task-detail-archive-actions">
            <button type="button" onClick={() => setConfirming(false)} disabled={busy}>
              Cancel
            </button>
            <button type="button" onClick={handleArchive} disabled={busy}>
              {busy ? "Archiving…" : "Archive task"}
            </button>
          </div>
        </div>
      ) : (
        <button type="button" onClick={() => setConfirming(true)}>
          Archive task
        </button>
      )}
    </section>
  );
}
