import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";

import {
  DndContext,
  DragOverlay,
  KeyboardCode,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import type {
  Announcements,
  DragEndEvent,
  DragStartEvent,
  UniqueIdentifier,
} from "@dnd-kit/core";
import { sortableKeyboardCoordinates } from "@dnd-kit/sortable";

import { createTask, toRimaiaError } from "../../lib/commands";
import { BOARD_COLUMNS, visibleColumns } from "../../lib/board";
import type { BoardCard, BoardColumns } from "../../lib/board";
import type { BoardColumn, RimaiaError, Task, TaskSummary } from "../../types";
import { useRepositories, useTasks } from "../../hooks/useTasks";
import { ErrorBanner } from "../ErrorBanner";
import { BoardToolbar } from "./BoardToolbar";
import { Column, COLUMN_TITLES } from "./Column";
import { TaskCardPreview } from "./TaskCard";
import { TaskDetailPanel } from "./TaskDetailPanel";

// ---------------------------------------------------------------------------
// Pure helpers — no React, no dnd-kit runtime state, kept here rather than
// component-local so the drop translation and the arrow-key navigation
// target can be tested by calling them directly (jsdom cannot simulate the
// pointer choreography that would otherwise be the only way to reach them).
// ---------------------------------------------------------------------------

/**
 * Where a dnd-kit `over.id` lands: the column itself (dropped on empty
 * space below every card — `over` is the column's own droppable id) or a
 * card (dropped near/over it — `over` is that card's id).
 *
 * The returned `index` is exactly `planMove`'s `toIndex` convention: dropped
 * on the column, `columns[column].length` is the post-self-removal "end of
 * list" regardless of whether the dragged card was already in this column
 * (`planMove` filters it out before clamping); dropped on a card, that
 * card's index in `columns[column]` *as currently displayed* is the index
 * dnd-kit's own `arrayMove` would use for the same drop, which is the
 * convention `planMove`'s own tests fix (see `lib/board.ts`).
 *
 * `crossColumnBelowMidpoint` only ever changes the result for a drop over a
 * card in a **different** column than the dragged card's own: dnd-kit's
 * `over` names which card the pointer is over, never whether it is above or
 * below it, so without this the bottom slot of another column is reachable
 * only through its own empty space (see the fix pass's finding 10) — a
 * column with no empty area, because every one of its cards is already
 * filled, would have no way to land last. A **same**-column reorder must
 * never apply it: that index is already the post-self-removal slot
 * (`planMove`'s own doc comment explains why), and adding this on top would
 * double-count the shift.
 */
export function resolveDrop(
  columns: BoardColumns<Task>,
  activeId: UniqueIdentifier,
  overId: UniqueIdentifier,
  crossColumnBelowMidpoint: boolean,
): { column: BoardColumn; index: number } | null {
  const id = String(overId);
  if (isBoardColumnId(id)) {
    return { column: id, index: columns[id].length };
  }

  const activeColumn = BOARD_COLUMNS.find((column) =>
    columns[column].some((card) => card.id === String(activeId)),
  );

  for (const column of BOARD_COLUMNS) {
    const index = columns[column].findIndex((card) => card.id === id);
    if (index === -1) continue;
    const crossColumn = activeColumn !== undefined && activeColumn !== column;
    const adjusted = crossColumn && crossColumnBelowMidpoint ? index + 1 : index;
    return { column, index: adjusted };
  }

  return null;
}

/**
 * Whether the dragged item's current rect sits below the midpoint of the
 * card it was dropped over — the one piece of `resolveDrop`'s cross-column
 * decision that genuinely needs real layout, so it lives here rather than in
 * `resolveDrop` itself, which stays a pure function a test can drive with
 * plain numbers. jsdom returns an all-zero rect for everything
 * (`getBoundingClientRect`), so this always reads `false` under jsdom —
 * consistent, not wrong, and nothing here claims otherwise (see the fix
 * pass's own note on why a jsdom drag proves nothing).
 */
function isBelowMidpoint(event: DragEndEvent): boolean {
  const dragged = event.active.rect.current.translated;
  const target = event.over?.rect;
  if (!dragged || !target) return false;
  return dragged.top + dragged.height / 2 > target.top + target.height / 2;
}

function isBoardColumnId(value: string): value is BoardColumn {
  return (BOARD_COLUMNS as readonly string[]).includes(value);
}

/** Generic in the card so the caller gets back what it put in — the board
 *  holds `TaskSummary` (seam-contract D12) and the card it renders needs the
 *  summary's own fields, while the helpers around it read nothing but ids. */
export function findCard<T extends BoardCard>(
  columns: BoardColumns<T>,
  id: string | null,
): T | null {
  if (!id) return null;
  for (const column of BOARD_COLUMNS) {
    const found = columns[column].find((card) => card.id === id);
    if (found) return found;
  }
  return null;
}

/**
 * What a screen reader should hear in place of a raw id: a card's title, or
 * a column's display name. dnd-kit's own default announcements read the
 * literal `active.id`/`over.id` — a task UUID (seam-contract D10) or the raw
 * column key (`not_ready`) — which is unusable to a screen-reader user
 * (fix pass finding 14). Handles both because `over.id` is either, and
 * `active.id` is always a card in this board.
 */
export function describeDragEntity(columns: BoardColumns<Task>, id: UniqueIdentifier): string {
  const key = String(id);
  if (isBoardColumnId(key)) return COLUMN_TITLES[key];
  return findCard(columns, key)?.title ?? key;
}

/**
 * The card arrow-key focus navigation should land on next, or `null` when
 * there is nowhere to go (top/bottom of a column, or the adjacent column has
 * no card). Up/down move within a column; left/right cross to the adjacent
 * column at the same row, clamped to its length — there is no ADR or task
 * wording pinning this shape down, it is the ordinary 2D-grid convention.
 */
export function nextFocusTarget(
  columns: BoardColumns<Task>,
  fromId: string,
  key: string,
): string | null {
  const currentColumn = BOARD_COLUMNS.find((column) =>
    columns[column].some((card) => card.id === fromId),
  );
  if (!currentColumn) return null;

  const list = columns[currentColumn];
  const index = list.findIndex((card) => card.id === fromId);

  if (key === "ArrowUp") return list[index - 1]?.id ?? null;
  if (key === "ArrowDown") return list[index + 1]?.id ?? null;

  if (key === "ArrowLeft" || key === "ArrowRight") {
    const columnIndex = BOARD_COLUMNS.indexOf(currentColumn);
    const nextColumn = BOARD_COLUMNS[columnIndex + (key === "ArrowLeft" ? -1 : 1)];
    if (!nextColumn) return null;
    const nextList = columns[nextColumn];
    if (nextList.length === 0) return null;
    return nextList[Math.min(index, nextList.length - 1)].id;
  }

  return null;
}

/** `n` and `/` must not fire while the user is typing anywhere editable —
 *  task 005's own wording for the plan textarea, generalised to every
 *  editable surface so it keeps holding once stage 3 adds one. */
export function isEditableTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  if (target.tagName === "INPUT" || target.tagName === "TEXTAREA") return true;
  // `isContentEditable` is the spec-correct check (it accounts for
  // inheritance from an ancestor), but jsdom implements neither it nor the
  // `contentEditable` IDL setter — the attribute is checked directly too, so
  // this holds in a real browser and in a test that can only set the
  // attribute.
  return target.isContentEditable || target.getAttribute("contenteditable") === "true";
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

export function Board() {
  const [repositoryFilter, setRepositoryFilter] = useState<string | null>(null);
  const { repositories, error: repositoriesError } = useRepositories();
  const { state, loading, readError, refresh, moveCard, dismissRejection } =
    useTasks(repositoryFilter);

  const [selectedTaskId, setSelectedTaskId] = useState<string | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [activeId, setActiveId] = useState<string | null>(null);
  const [createError, setCreateError] = useState<RimaiaError | null>(null);

  // Re-renders every 30s so "relative time of last activity" does not go
  // stale while a card just sits there — a UI refresh tick, not a fake-able
  // clock a test asserts against (CLAUDE.md's "fake the clock" rule is about
  // deterministic logic tests; `lib/board.ts`'s `relativeTime` already takes
  // `now` as a parameter for exactly that reason).
  const [, tick] = useReducer((count: number) => count + 1, 0);
  useEffect(() => {
    const id = setInterval(tick, 30_000);
    return () => clearInterval(id);
  }, []);
  const now = new Date();

  const searchInputRef = useRef<HTMLInputElement>(null);
  const cardRefs = useRef(new Map<string, HTMLElement>());
  const registerCardRef = useCallback((id: string, element: HTMLElement | null) => {
    if (element) cardRefs.current.set(id, element);
    else cardRefs.current.delete(id);
  }, []);

  const columns = visibleColumns(state);

  const repositoryNamesById = useMemo(() => {
    const map = new Map<string, string>();
    for (const repository of repositories) map.set(repository.id, repository.name);
    return map;
  }, [repositories]);

  const query = searchQuery.trim().toLowerCase();
  const dragDisabled = query !== "";
  const visibleCards = useCallback(
    (column: BoardColumn): readonly TaskSummary[] =>
      query === ""
        ? columns[column]
        : columns[column].filter((card) => card.title.toLowerCase().includes(query)),
    [columns, query],
  );

  // What arrow-key focus navigation actually walks: only cards a search
  // filter has not hidden have a registered ref to focus (`Column` renders
  // `visibleCards`, not `columns`) - navigating against the unfiltered board
  // computed a target with no ref to land on, and silently did nothing
  // (fix pass finding 4).
  const filteredColumns: BoardColumns<TaskSummary> = useMemo(() => {
    const result: Record<BoardColumn, readonly TaskSummary[]> = {
      not_ready: [],
      ready: [],
      in_review: [],
      done: [],
    };
    for (const column of BOARD_COLUMNS) result[column] = visibleCards(column);
    return result;
  }, [visibleCards]);

  const selectedTask = findCard(columns, selectedTaskId);
  useEffect(() => {
    // Closed by whatever made it disappear — deleted elsewhere, or filtered
    // out by a repository switch — rather than showing stale content.
    if (selectedTaskId && !selectedTask) setSelectedTaskId(null);
  }, [selectedTaskId, selectedTask]);

  // The other half of the detail panel's focus handling: it is an overlay
  // drawer that takes focus when it opens, so closing it has to give focus
  // back rather than drop it on `<body>`, where the next Tab restarts from
  // the top of the app. Only on the open -> closed transition: switching
  // from one card to another must leave focus in the panel that stayed open.
  // A card that no longer exists (just deleted) simply has no ref to focus.
  const lastSelectedTaskId = useRef<string | null>(null);
  useEffect(() => {
    const previous = lastSelectedTaskId.current;
    lastSelectedTaskId.current = selectedTaskId;
    if (previous && !selectedTaskId) cardRefs.current.get(previous)?.focus();
  }, [selectedTaskId]);

  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
      // dnd-kit's own default also claims Enter to lift/drop, which leaves
      // no key free for a focused card to open the detail panel (fix pass
      // finding 8) - narrowed to Space alone, matching the on-screen
      // instructions dnd-kit itself renders ("press the space bar").
      keyboardCodes: {
        start: [KeyboardCode.Space],
        cancel: [KeyboardCode.Esc],
        end: [KeyboardCode.Space],
      },
    }),
  );

  const announcements: Announcements = useMemo(
    () => ({
      onDragStart: ({ active }) => `Picked up "${describeDragEntity(columns, active.id)}".`,
      onDragOver: ({ active, over }) =>
        over
          ? `"${describeDragEntity(columns, active.id)}" was moved over "${describeDragEntity(columns, over.id)}".`
          : `"${describeDragEntity(columns, active.id)}" is no longer over a droppable area.`,
      onDragEnd: ({ active, over }) =>
        over
          ? `"${describeDragEntity(columns, active.id)}" was dropped over "${describeDragEntity(columns, over.id)}".`
          : `"${describeDragEntity(columns, active.id)}" was dropped.`,
      onDragCancel: ({ active }) => `Moving "${describeDragEntity(columns, active.id)}" was cancelled.`,
    }),
    [columns],
  );

  function handleDragStart(event: DragStartEvent) {
    setActiveId(String(event.active.id));
  }

  function handleDragEnd(event: DragEndEvent) {
    setActiveId(null);
    if (!event.over) return;
    const drop = resolveDrop(columns, event.active.id, event.over.id, isBelowMidpoint(event));
    if (!drop) return;
    moveCard(String(event.active.id), drop.column, drop.index);
  }

  function handleDragCancel() {
    setActiveId(null);
  }

  function handleArrowNavigate(fromId: string, key: string) {
    const targetId = nextFocusTarget(filteredColumns, fromId, key);
    if (targetId) cardRefs.current.get(targetId)?.focus();
  }

  const handleNewTask = useCallback(() => {
    if (repositories.length === 0) return;
    const targetRepositoryId = repositoryFilter ?? repositories[0].id;
    setCreateError(null);
    createTask({ repositoryId: targetRepositoryId, title: "Untitled task" }).then(
      // Wait for the refreshed list to land before selecting the new task —
      // selecting first and refreshing after would race the "close the panel
      // if its task is not in `state.tasks`" effect below, which cannot tell
      // "deleted" apart from "not fetched yet" on the render in between.
      (created) => refresh().then(() => setSelectedTaskId(created.id)),
      (thrown) => setCreateError(toRimaiaError(thrown)),
    );
  }, [repositories, repositoryFilter, refresh]);

  useEffect(() => {
    function handleKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        // A keyboard drag in progress owns Escape first - dnd-kit's own
        // `KeyboardSensor` cancels the drag on it. Without this guard the
        // same keypress also closed the panel underneath the drag (fix pass
        // finding 9); `activeId` is more direct than trusting
        // `defaultPrevented`, which depends on dnd-kit's document-level
        // listener having run before this window-level one does.
        if (activeId) return;
        if (selectedTaskId) {
          setSelectedTaskId(null);
          return;
        }
        if (document.activeElement instanceof HTMLElement && isEditableTarget(event.target)) {
          document.activeElement.blur();
        }
        return;
      }

      // Every other shortcut below is a bare letter or `/` — never bind
      // those while the user is typing, or typing becomes impossible.
      if (isEditableTarget(event.target)) return;

      if (event.key === "/") {
        event.preventDefault();
        searchInputRef.current?.focus();
        return;
      }

      if (event.key === "n") {
        event.preventDefault();
        handleNewTask();
      }
    }

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [selectedTaskId, handleNewTask, activeId]);

  const activeCard = findCard(columns, activeId);

  return (
    <div className="board-view">
      <BoardToolbar
        repositories={repositories}
        selectedRepositoryId={repositoryFilter}
        onRepositoryChange={setRepositoryFilter}
        searchQuery={searchQuery}
        onSearchChange={setSearchQuery}
        searchInputRef={searchInputRef}
        onNewTask={handleNewTask}
        newTaskDisabled={repositories.length === 0}
      />

      {repositoriesError && <ErrorBanner error={repositoriesError} />}
      {readError && <ErrorBanner error={readError} onDismiss={refresh} />}
      {createError && (
        <ErrorBanner error={createError} onDismiss={() => setCreateError(null)} />
      )}
      {state.rejection && (
        <ErrorBanner
          error={{ code: "invalid", message: state.rejection.reason }}
          onDismiss={dismissRejection}
        />
      )}
      {loading && state.tasks.length === 0 && <p className="muted">Reading tasks…</p>}

      <div className="board-layout">
        <DndContext
          sensors={sensors}
          accessibility={{ announcements }}
          onDragStart={handleDragStart}
          onDragEnd={handleDragEnd}
          onDragCancel={handleDragCancel}
        >
          <div className="board-columns">
            {BOARD_COLUMNS.map((column) => (
              <Column
                key={column}
                column={column}
                cards={visibleCards(column)}
                repositoriesById={repositoryNamesById}
                selectedTaskId={selectedTaskId}
                onSelect={setSelectedTaskId}
                registerCardRef={registerCardRef}
                onArrowNavigate={handleArrowNavigate}
                dragDisabled={dragDisabled}
                now={now}
              />
            ))}
          </div>
          <DragOverlay>
            {activeCard ? (
              <TaskCardPreview
                task={activeCard}
                repositoryName={
                  repositoryNamesById.get(activeCard.repositoryId) ?? activeCard.repositoryId
                }
                now={now}
              />
            ) : null}
          </DragOverlay>
        </DndContext>

        {selectedTask && (
          <TaskDetailPanel
            task={selectedTask}
            repositoryName={
              repositoryNamesById.get(selectedTask.repositoryId) ?? selectedTask.repositoryId
            }
            repositories={repositories}
            onClose={() => setSelectedTaskId(null)}
          />
        )}
      </div>
    </div>
  );
}
