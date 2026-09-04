import { useDroppable } from "@dnd-kit/core";
import { SortableContext, verticalListSortingStrategy } from "@dnd-kit/sortable";

import { cardBadge } from "../../lib/board";
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

/** One line of a column's pulse: how many of its cards are in one state that
 *  is not "nothing is happening". */
export interface ColumnStat {
  readonly tone: "running" | "queued" | "blocked" | "waiting" | "failed";
  readonly count: number;
  readonly label: string;
}

/**
 * Most urgent first, and the order the header renders them in. `failed` leads
 * for the same reason ADR-0007 keeps a failure visible rather than letting a
 * later state paper over it: at 08:00 it is the only one of the five that
 * definitely needs a human.
 */
const STAT_ORDER: readonly ColumnStat["tone"][] = [
  "failed",
  "waiting",
  "blocked",
  "running",
  "queued",
];

const STAT_LABELS: Record<ColumnStat["tone"], string> = {
  failed: "failed",
  waiting: "waiting",
  blocked: "blocked",
  running: "running",
  queued: "queued",
};

/**
 * What a column header says about its cards beyond how many there are.
 *
 * Derived through `cardBadge`, not off `runState` directly, so the header and
 * the badges underneath it can never disagree — a `failed` run that D9 renders
 * as `interrupted`, or an `idle` task ADR-0008 derives `blocked` for, is
 * counted as what the card actually shows.
 *
 * **At most three kinds**, most urgent first. The whole point of an aggregate
 * on a 160px-wide header is that a healthy board renders nothing here; five
 * coloured chips on every column would be the noise this is meant to replace.
 * Nothing is silently dropped that a user could act on: the fourth and fifth
 * kinds are always the two calmest ones (`running`, `queued`), and every card
 * still carries its own badge in the tray below.
 *
 * Deliberately **not** a cost total, which would be the other obvious readout:
 * `TaskSummary.lastRun` (seam-contract D12) carries a status and an exit class
 * and no `costUsd` at all, so a per-column spend would have to be invented on
 * the frontend or fetched per card.
 */
export function columnStats(cards: readonly ColumnCard[]): readonly ColumnStat[] {
  const counts: Record<ColumnStat["tone"], number> = {
    failed: 0,
    waiting: 0,
    blocked: 0,
    running: 0,
    queued: 0,
  };

  for (const card of cards) {
    switch (cardBadge(card.runState, card.lastRun ?? null, card.blockedByIncomplete ?? false)) {
      case "running":
        counts.running += 1;
        break;
      case "queued":
        counts.queued += 1;
        break;
      case "blocked":
        counts.blocked += 1;
        break;
      case "waiting_retry":
        counts.waiting += 1;
        break;
      case "failed":
      case "interrupted":
        counts.failed += 1;
        break;
      // `cancelled` and `null` (idle) are both "nothing is happening", which
      // is what an absent chip already says.
    }
  }

  return STAT_ORDER.filter((tone) => counts[tone] > 0)
    .slice(0, 3)
    .map((tone) => ({ tone, count: counts[tone], label: STAT_LABELS[tone] }));
}

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
  const stats = columnStats(cards);

  return (
    <section className={`board-column board-column-${column}`} aria-label={COLUMN_TITLES[column]}>
      {/* An instrument readout rather than a title with a pill beside it: the
          name is furniture (uppercase eyebrow), the count is the measurement
          (`--font-size-lg`, tabular so four columns compare at a glance), and
          the pulse underneath is what changed overnight. */}
      <header className="board-column-header">
        <div className="board-column-heading">
          <h3>{COLUMN_TITLES[column]}</h3>
          <span className="board-column-count tabular-nums">{cards.length}</span>
        </div>
        {/* ADR-0007's "board order *is* execution order", said once where the
            queue actually reads from, instead of being a fact you have to know
            already. */}
        {column === "ready" && <p className="board-column-note">The run queue, top to bottom</p>}
        {stats.length > 0 && (
          <p className="board-column-stats">
            {stats.map((stat) => (
              <span key={stat.tone} className={`board-column-stat board-column-stat-${stat.tone}`}>
                <span
                  className={stat.tone === "running" ? "status-dot status-dot-live" : "status-dot"}
                  aria-hidden="true"
                />
                <span className="tabular-nums">{stat.count}</span> {stat.label}
              </span>
            ))}
          </p>
        )}
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
