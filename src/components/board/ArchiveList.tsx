import { useState } from "react";

import { toRimaiaError, unarchiveTask } from "../../lib/commands";
import { relativeTime } from "../../lib/board";
import type { RimaiaError, TaskSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { COLUMN_TITLES } from "./Column";

interface ArchiveListProps {
  readonly cards: readonly TaskSummary[];
  readonly repositoriesById: ReadonlyMap<string, string>;
  readonly now: Date;
  /** Opens the same detail panel the board opens, so an archived task is read
   *  and edited in one place rather than in a read-only copy of it. */
  readonly onSelect: (id: string) => void;
  readonly onUnarchived: () => void;
}

/**
 * ADR-0025's archive, as a flat list rather than four columns.
 *
 * Flat because the question here is "when did I put this down", not "where is
 * it in my process" — the column is still shown, since an archived card still
 * has one and unarchiving returns it there, but it is a fact about the row
 * rather than the axis the list is laid out along.
 *
 * It reads the same `list_tasks` the board does, with the filter flipped; that
 * is why there is no second read, no second projection and no second ordering
 * rule to keep in step (seam-contract D26.1 and D26.2).
 */
export function ArchiveList({
  cards,
  repositoriesById,
  now,
  onSelect,
  onUnarchived,
}: ArchiveListProps) {
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  function handleUnarchive(id: string) {
    setBusyId(id);
    setError(null);
    unarchiveTask(id).then(
      () => {
        setBusyId(null);
        onUnarchived();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setBusyId(null);
      },
    );
  }

  if (cards.length === 0) {
    return (
      <div className="archive-list archive-list-empty">
        <p className="muted">
          Nothing archived. Archiving takes a card off the board and keeps everything —
          its runs, its links and whatever depends on it.
        </p>
      </div>
    );
  }

  return (
    <div className="archive-list">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <ul className="archive-rows">
        {cards.map((card) => (
          <li key={card.id} className="archive-row">
            <button
              type="button"
              className="archive-row-title"
              onClick={() => onSelect(card.id)}
            >
              {card.title}
            </button>
            <span className="archive-row-meta muted">
              {repositoriesById.get(card.repositoryId) ?? card.repositoryId} ·{" "}
              {COLUMN_TITLES[card.column]}
              {card.archivedAt && ` · archived ${relativeTime(card.archivedAt, now)}`}
            </span>
            <button
              type="button"
              className="archive-row-restore"
              onClick={() => handleUnarchive(card.id)}
              disabled={busyId === card.id}
            >
              {busyId === card.id ? "Unarchiving…" : "Unarchive"}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}
