import { useDroppable } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";

import type { BoardColumn as BoardColumnId, EffectiveStrategyFields, Task, TaskSummary } from "../../types";
import { TaskCard } from "./TaskCard";

/** Task 005's display names, ADR-0007's board order. */
export const COLUMN_TITLES: Record<BoardColumnId, string> = {
  not_ready: "Not ready for implementation",
  ready: "Ready for implementation",
  in_review: "In review",
  done: "Done",
};

/** What an empty column says belongs there — task 005's "empty column states
 *  that say what belongs there", not a blank box. */
export const COLUMN_EMPTY_HINTS: Record<BoardColumnId, string> = {
  not_ready: "Captured ideas without a finished plan land here.",
  ready:
    "Nothing queued. Cards with a finished plan go here — the run queue works this column top to bottom.",
  in_review: "Nothing waiting on you. Successful runs land here for review.",
  done: "Nothing finished yet. Reviewed and accepted tasks land here.",
};

/**
 * What a column will pass to a card: `Board` hands it `TaskSummary` rows
 * (seam-contract D12) and this file's own tests hand it bare `Task`s, so the
 * summary fields are optional here exactly as they are in `TaskCard`'s
 * `CardTask`.
 *
 * Widened from a bare `readonly Task[]` by task 011. It had been narrower than
 * what `Board` actually passes ever since D12 landed, which cost nothing while
 * the extra fields were only ever *read* through `??` defaults — but a card
 * that must name its blocker cannot be given a type that has erased the name.
 */
type ColumnCard = Task &
  Partial<
    Pick<
      TaskSummary,
      "linkCount" | "dependencyCount" | "lastRun" | "blockedByIncomplete" | "blockingTitle"
    >
  > &
  Partial<EffectiveStrategyFields>;

interface ColumnProps {
  readonly column: BoardColumnId;
  readonly cards: readonly ColumnCard[];
  readonly repositoriesById: ReadonlyMap<string, string>;
  readonly selectedTaskId: string | null;
  readonly onSelect: (id: string) => void;
  readonly registerCardRef: (id: string, element: HTMLElement | null) => void;
  readonly onArrowNavigate: (id: string, key: string) => void;
  /** True while a title search is filtering the board — dragging while some
   *  cards are hidden would compute neighbours against a list the user
   *  cannot see, so the whole column-empty-area drop target and every card
   *  in it are made inert instead. */
  readonly dragDisabled: boolean;
  readonly now: Date;
}

export function Column({
  column,
  cards,
  repositoriesById,
  selectedTaskId,
  onSelect,
  registerCardRef,
  onArrowNavigate,
  dragDisabled,
  now,
}: ColumnProps) {
  // A column with zero cards is still a legal drop target — `useDroppable`
  // on the body is what makes an *empty* column droppable, since there is no
  // card in it for `SortableContext`'s own item rects to catch a drop on.
  const { setNodeRef, isOver } = useDroppable({ id: column, disabled: dragDisabled });
  const ids = cards.map((card) => card.id);

  return (
    <section className="board-column" aria-label={COLUMN_TITLES[column]}>
      <header className="board-column-header">
        <h3>{COLUMN_TITLES[column]}</h3>
        <span className="board-column-count">{cards.length}</span>
      </header>
      <div
        ref={setNodeRef}
        className={`board-column-body${isOver ? " board-column-body-over" : ""}`}
      >
        <SortableContext items={ids} strategy={verticalListSortingStrategy} disabled={dragDisabled}>
          {cards.length === 0 ? (
            <p className="board-column-empty">{COLUMN_EMPTY_HINTS[column]}</p>
          ) : (
            cards.map((card) => (
              <TaskCard
                key={card.id}
                task={card}
                repositoryName={repositoriesById.get(card.repositoryId) ?? card.repositoryId}
                now={now}
                selected={card.id === selectedTaskId}
                onSelect={onSelect}
                registerCardRef={registerCardRef}
                onArrowNavigate={onArrowNavigate}
              />
            ))
          )}
        </SortableContext>
      </div>
    </section>
  );
}
