import type { BoardColumn, ExitClass, RunState } from "../types";

/**
 * Board ordering, the optimistic move overlay and the derived card state —
 * everything about task 005 that is true without a browser.
 *
 * It is a module rather than component-local logic because jsdom has no layout
 * engine: a test that drives a drag there proves nothing, so the part ordering
 * actually depends on — ADR-0007's "board order *is* execution order", the one
 * load-bearing interaction on this screen — is extracted to where a green test
 * means something. The components above this file render what these functions
 * return and decide nothing themselves.
 *
 * **Nothing here computes a `position`** (seam-contract D1). A drop is
 * translated into the `beforeId`/`afterId` pair `move_task` takes and the
 * number comes back from the service; the optimistic overlay is a list splice,
 * not a synthesised midpoint, for the same reason. If a diff in this file grows
 * a division by two, it has misread D1.
 */

/** ADR-0007's four columns, in board order. There is no fifth. */
export const BOARD_COLUMNS = [
  "not_ready",
  "ready",
  "in_review",
  "done",
] as const satisfies readonly BoardColumn[];

/**
 * The least a card has to carry to be placed on the board: what
 * {@link groupIntoColumns} sorts by, plus what a settled move writes back.
 *
 * Structural rather than `Task`, so the component can hold whatever shape it
 * ends up fetching — `Task` from `listTasks` and `TaskDetail` from `getTask`
 * both satisfy it — and the ordering functions stay honest about the three
 * fields they actually read.
 */
export interface BoardCard {
  id: string;
  repositoryId: string;
  column: BoardColumn;
  position: number;
  createdAt: string;
  updatedAt: string;
}

/** Every column, always all four, each in board order. */
export type BoardColumns<T> = Readonly<Record<BoardColumn, readonly T[]>>;

/**
 * Groups tasks into ADR-0007's four columns, each in the order the board
 * shows it.
 *
 * A card whose `column` is none of the four is dropped rather than thrown on.
 * The value is `CHECK`-constrained in the schema, so the only way to see one
 * is a database somebody edited by hand — which ADR-0003 says will happen, and
 * which should cost that one card, not the whole board.
 */
export function groupIntoColumns<T extends BoardCard>(tasks: readonly T[]): BoardColumns<T> {
  const grouped: Record<BoardColumn, T[]> = {
    not_ready: [],
    ready: [],
    in_review: [],
    done: [],
  };

  for (const task of tasks) {
    if (isBoardColumn(task.column)) {
      grouped[task.column].push(task);
    }
  }
  for (const column of BOARD_COLUMNS) {
    grouped[column].sort(compareBoardOrder);
  }

  return grouped;
}

function isBoardColumn(value: string): value is BoardColumn {
  return (BOARD_COLUMNS as readonly string[]).includes(value);
}

/**
 * The order the backend already returns rows in — `list_tasks`'s
 * `repository_id, position, created_at, id`, minus the column the grouping
 * above handles.
 *
 * `repositoryId` leads because a `position` is only comparable within one
 * `(repository, column)` (ADR-0007): with the board showing every repository,
 * sorting on position alone would interleave numbers that mean nothing to each
 * other. Keeping each repository's cards contiguous is also what lets
 * {@link planMove}'s neighbour search land where the user dropped. With one
 * repository selected the term is inert.
 *
 * `createdAt` then `id` break a position tie. Ties are possible — the schema
 * does not make `position` unique, which is the whole reason
 * `tasks::position::rebalance_column` exists — and these are the same two
 * tiebreaks that function renumbers by, so a column with ties reads the same
 * before and after a repair.
 *
 * All three strings compare lexicographically because all three are ASCII: uuid
 * text (seam-contract D10) and the RFC 3339 UTC timestamps sqlx writes, whose
 * lexical order is their chronological one.
 */
function compareBoardOrder(a: BoardCard, b: BoardCard): number {
  if (a.repositoryId !== b.repositoryId) return a.repositoryId < b.repositoryId ? -1 : 1;
  if (a.position !== b.position) return a.position - b.position;
  if (a.createdAt !== b.createdAt) return a.createdAt < b.createdAt ? -1 : 1;
  if (a.id !== b.id) return a.id < b.id ? -1 : 1;
  return 0;
}

// ---------------------------------------------------------------------------
// The drop translation
// ---------------------------------------------------------------------------

/**
 * One drop, in the two vocabularies it has to be expressed in.
 *
 * `column`, `beforeId` and `afterId` are exactly `move_task`'s arguments;
 * `displayIndex` is the optimistic overlay's only input and never crosses the
 * IPC boundary.
 */
export interface PlannedMove {
  readonly taskId: string;
  readonly column: BoardColumn;
  /** The card that ends up **above** — `move_task`'s `before_id`. */
  readonly beforeId: string | null;
  /** The card that ends up **below** — `move_task`'s `after_id`. */
  readonly afterId: string | null;
  /**
   * Where the card lands in the destination column as displayed. Same index
   * convention as `toIndex` in {@link planMove}, and the optimistic overlay's
   * fallback for the one drop that names no neighbour at all — see
   * {@link applyPendingMoves}.
   */
  readonly displayIndex: number;
}

/**
 * Translates "this card was dropped here" into the `move_task` call that says
 * so.
 *
 * `toIndex` is **the index the card will occupy in `toColumn` once it has been
 * removed from wherever it was** — dnd-kit's `arrayMove` convention, and the
 * one definition that reads the same for a reorder and a cross-column move.
 * Getting this backwards is the off-by-one in this task: dragging a card *down*
 * its own column shifts every later card up by one when the card leaves, so an
 * index into the column as it looked *before* the drag names the wrong
 * neighbours by exactly one slot. `columns[toColumn]` is filtered before it is
 * indexed here, which is where that is handled once.
 *
 * Returns `null` when there is nothing to send: the board does not hold
 * `taskId`, or the drop leaves the card exactly where it already is. The
 * no-op test compares *neighbours*, not indices — a drop that changes only
 * which of another repository's cards the card is nominally next to changes no
 * order, and firing `move_task` for it would stamp `updated_at` and wake every
 * other window for nothing.
 *
 * Neighbours are searched among the dragged card's **own repository**. That is
 * not a preference: `move_task` looks a neighbour up by
 * `(id, repository_id, board_column)` and refuses one from another repository,
 * so with the board showing every repository this projection is the only legal
 * call the drop can become. Because `compareBoardOrder` keeps each
 * repository's cards contiguous, the search only ever walks to the edge of the
 * card's own block, so what it names is what the user saw.
 *
 * Naming neither neighbour is `move_task`'s "the destination column is empty"
 * case, which it accepts and otherwise refuses; the same-repository projection
 * makes that pair unreachable in exactly the cases it would be refused.
 *
 * The empty-plan rule for landing in `ready` is deliberately **not** checked
 * here. It is a business rule and business rules live in `rimaia-core`
 * (ADR-0006); the drop is sent, the service refuses it, and the rejection path
 * shows the reason it gives.
 */
export function planMove<T extends BoardCard>(
  columns: BoardColumns<T>,
  taskId: string,
  toColumn: BoardColumn,
  toIndex: number,
): PlannedMove | null {
  const located = locate(columns, taskId);
  if (!located) return null;

  const displayed = columns[toColumn].filter((card) => card.id !== taskId);
  const displayIndex = clampIndex(toIndex, displayed.length);
  const dropped = neighboursAt(displayed, displayIndex, located.card.repositoryId);

  if (toColumn === located.column) {
    // A card's index in the column it is already in doubles as the index it
    // would be reinserted at to stay put, because removing it shifts exactly
    // the cards below it.
    const staying = neighboursAt(displayed, located.index, located.card.repositoryId);
    if (staying.beforeId === dropped.beforeId && staying.afterId === dropped.afterId) {
      return null;
    }
  }

  return {
    taskId,
    column: toColumn,
    beforeId: dropped.beforeId,
    afterId: dropped.afterId,
    displayIndex,
  };
}

interface Located<T> {
  readonly card: T;
  readonly column: BoardColumn;
  readonly index: number;
}

/** Which column list actually holds the card, which is the truth the overlay
 *  needs — a card's own `column` field can lag a splice by one render. */
function locate<T extends BoardCard>(
  columns: BoardColumns<T>,
  taskId: string,
): Located<T> | null {
  for (const column of BOARD_COLUMNS) {
    const index = columns[column].findIndex((card) => card.id === taskId);
    if (index !== -1) {
      return { card: columns[column][index], column, index };
    }
  }
  return null;
}

/** The nearest card of `repositoryId` above and below an insertion slot. */
function neighboursAt<T extends BoardCard>(
  displayed: readonly T[],
  index: number,
  repositoryId: string,
): { beforeId: string | null; afterId: string | null } {
  let beforeId: string | null = null;
  for (let above = index - 1; above >= 0; above -= 1) {
    if (displayed[above].repositoryId === repositoryId) {
      beforeId = displayed[above].id;
      break;
    }
  }

  let afterId: string | null = null;
  for (let below = index; below < displayed.length; below += 1) {
    if (displayed[below].repositoryId === repositoryId) {
      afterId = displayed[below].id;
      break;
    }
  }

  return { beforeId, afterId };
}

function clampIndex(index: number, length: number): number {
  return Math.min(Math.max(Math.trunc(index), 0), length);
}

// ---------------------------------------------------------------------------
// Optimistic application and reconciliation
// ---------------------------------------------------------------------------

/** A move sent to `move_task` and not yet answered. */
export interface PendingMove {
  /** Correlates the response with the move; see {@link BoardState.nextSeq}. */
  readonly seq: number;
  readonly move: PlannedMove;
}

/** Why the board snapped a card back, until the user's next drag or a dismiss. */
export interface MoveRejection {
  readonly taskId: string;
  /** The `RimaiaError.message` the service refused with, rendered verbatim —
   *  seam-contract D8 puts the specificity in the message, not the code. */
  readonly reason: string;
}

/**
 * What a settled `move_task` tells the board: where the row actually landed.
 * `Task` satisfies this structurally, so the command's return value is passed
 * straight in — and only these fields are merged, so handing back a `Task`
 * never clobbers the links or last run of a `TaskDetail` the board is holding.
 */
export interface MovePlacement {
  readonly id: string;
  readonly column: BoardColumn;
  readonly position: number;
  readonly updatedAt: string;
}

export interface BoardState<T extends BoardCard> {
  /** Server truth, as last read. Never optimistically edited. */
  readonly tasks: readonly T[];
  /** In-flight moves, oldest first. {@link visibleColumns} lays them over
   *  `tasks` in that order. */
  readonly pending: readonly PendingMove[];
  readonly rejection: MoveRejection | null;
  /** The `seq` the next `move_started` must carry. Allocated by the reducer so
   *  a caller never invents one that collides. */
  readonly nextSeq: number;
}

export function initialBoardState<T extends BoardCard>(
  tasks: readonly T[] = [],
): BoardState<T> {
  return { tasks, pending: [], rejection: null, nextSeq: 1 };
}

export type BoardAction<T extends BoardCard> =
  | { readonly kind: "tasks_read"; readonly tasks: readonly T[] }
  | { readonly kind: "move_started"; readonly seq: number; readonly move: PlannedMove }
  | { readonly kind: "move_settled"; readonly seq: number; readonly placed: MovePlacement }
  | { readonly kind: "move_rejected"; readonly seq: number; readonly reason: string }
  | { readonly kind: "rejection_dismissed" };

/**
 * The whole optimistic-move policy, as data.
 *
 * Three rules, each of which exists because the obvious alternative is a bug:
 *
 * 1. **A re-read replaces server truth and never clears a pending move.**
 *    `tasks:changed` fires for every mutation (ADR-0018), including ones this
 *    board caused, so a re-read routinely lands while a drag's own `move_task`
 *    is still in flight — and it lands *before* that call commits at least as
 *    often as after. An implementation that drops the optimistic state on a
 *    re-read therefore snaps the card back to where it was and then forward
 *    again when the response arrives, twice per drag. Because the overlay is
 *    re-applied on top of whatever was read, a stale read here costs nothing:
 *    the card cannot be clobbered by a read, only by its own response.
 *
 * 2. **A pending move is cleared only by its own `seq`.** Two drags in flight
 *    are two overlays applied in the order they were issued, and the second
 *    survives the first settling.
 *
 * 3. **A rejection discards the rejected move and every move issued after
 *    it.** Those later drops were planned against a board arrangement that
 *    never existed, so their neighbour ids describe an order the server never
 *    had. Discarding them shows the user the truth immediately; each still
 *    gets its own response, and `move_settled` merges any that succeeded
 *    anyway.
 */
export function boardReducer<T extends BoardCard>(
  state: BoardState<T>,
  action: BoardAction<T>,
): BoardState<T> {
  switch (action.kind) {
    case "tasks_read":
      return { ...state, tasks: action.tasks };

    case "move_started":
      return {
        ...state,
        pending: [...state.pending, { seq: action.seq, move: action.move }],
        rejection: null,
        nextSeq: Math.max(state.nextSeq, action.seq + 1),
      };

    case "move_settled":
      // The placement is merged whether or not the move is still pending: a
      // committed row is truth even for a move rule 3 already discarded.
      // Merging is not always *enough* — see
      // {@link settlementReproducesMove}, which is what the caller asks
      // before trusting it.
      return {
        ...state,
        tasks: state.tasks.map((task) => place(task, action.placed)),
        pending: state.pending.filter((entry) => entry.seq !== action.seq),
      };

    case "move_rejected": {
      const rejected = state.pending.find((entry) => entry.seq === action.seq);
      // Nothing pending under that seq means rule 3 already discarded it, and
      // the earlier rejection's reason is the one that explains the snap-back.
      if (!rejected) return state;
      return {
        ...state,
        pending: state.pending.filter((entry) => entry.seq < action.seq),
        rejection: { taskId: rejected.move.taskId, reason: action.reason },
      };
    }

    case "rejection_dismissed":
      return state.rejection === null ? state : { ...state, rejection: null };
  }
}

/**
 * Whether server truth, after `move_settled` merged the row `move_task`
 * answered with, actually puts the card where the drop asked for it.
 *
 * `move_task` returns the one row it moved, but when the gap between the
 * named neighbours has closed it renumbers **every** card in the destination
 * column (`tasks::position::rebalance_column`, seam-contract D1). Merging one
 * post-rebalance number into siblings still holding their pre-rebalance ones
 * orders that column by two incompatible scales at once — and under ADR-0007
 * the order the board shows is the order the queue claims it will run, so a
 * card that renders at the top of `ready` is a claim about what runs next.
 * A settlement this returns `false` for is one the board cannot reconcile by
 * merging; the only information that fixes it is a fresh read.
 *
 * Compares **neighbours**, not positions: neighbours are what the drop asked
 * for, and which number the service chose to express them with is its own
 * business (seam-contract D1). Any number that leaves the card between the
 * same two cards settled the move faithfully.
 *
 * `false` also covers the card having been deleted or moved elsewhere by
 * another window while the call was in flight — likewise only a read can say
 * what the board should show now.
 */
export function settlementReproducesMove<T extends BoardCard>(
  tasks: readonly T[],
  move: PlannedMove,
): boolean {
  const columns = groupIntoColumns(tasks);
  const located = locate(columns, move.taskId);
  if (!located || located.column !== move.column) return false;

  // Same shape as `planMove`'s own "would this drop change anything" test: a
  // card's index in the column it is in is the slot it occupies once it is
  // taken out of the list.
  const displayed = columns[located.column].filter((card) => card.id !== move.taskId);
  const settled = neighboursAt(displayed, located.index, located.card.repositoryId);
  return settled.beforeId === move.beforeId && settled.afterId === move.afterId;
}

function place<T extends BoardCard>(task: T, placed: MovePlacement): T {
  if (task.id !== placed.id) return task;
  return {
    ...task,
    column: placed.column,
    position: placed.position,
    updatedAt: placed.updatedAt,
  };
}

/** The board as the user should see it right now: server order with every
 *  in-flight move laid over it. */
export function visibleColumns<T extends BoardCard>(state: BoardState<T>): BoardColumns<T> {
  return applyPendingMoves(groupIntoColumns(state.tasks), state.pending);
}

/**
 * Lays pending moves over grouped columns, oldest first.
 *
 * A splice, not arithmetic: the card is removed from the list that holds it and
 * reinserted beside the neighbours the move named, and its `position` is left
 * alone. The frontend never computes a position (seam-contract D1), and while a
 * move is in flight there is no correct number to compute — the list order is
 * what renders until the service answers with the real one.
 *
 * The slot is re-derived from `beforeId`/`afterId` on every render rather than
 * taken from the drop index, so a card another window inserted above the slot
 * in the meantime does not drag the pending card up with it. `displayIndex` is
 * the fallback for the one drop that names no neighbour — into a column with
 * none of this repository's cards in it — where there is nothing to re-derive
 * from and the index is exactly what the user aimed at.
 */
export function applyPendingMoves<T extends BoardCard>(
  columns: BoardColumns<T>,
  pending: readonly PendingMove[],
): BoardColumns<T> {
  return pending.reduce<BoardColumns<T>>(
    (current, entry) => applyPlannedMove(current, entry.move),
    columns,
  );
}

function applyPlannedMove<T extends BoardCard>(
  columns: BoardColumns<T>,
  move: PlannedMove,
): BoardColumns<T> {
  const located = locate(columns, move.taskId);
  // Deleted from another window while the drag was in flight (ADR-0018): the
  // move has nothing left to describe, and the response will clear it.
  if (!located) return columns;

  const next: Record<BoardColumn, T[]> = {
    not_ready: [...columns.not_ready],
    ready: [...columns.ready],
    in_review: [...columns.in_review],
    done: [...columns.done],
  };

  next[located.column].splice(located.index, 1);
  const destination = next[move.column];
  destination.splice(insertionIndex(destination, move), 0, {
    ...located.card,
    column: move.column,
  });

  return next;
}

function insertionIndex<T extends BoardCard>(
  destination: readonly T[],
  move: PlannedMove,
): number {
  const below = destination.findIndex((card) => card.id === move.afterId);
  if (below !== -1) return below;

  const above = destination.findIndex((card) => card.id === move.beforeId);
  if (above !== -1) return above + 1;

  return clampIndex(move.displayIndex, destination.length);
}

// ---------------------------------------------------------------------------
// Derived card state
// ---------------------------------------------------------------------------

/**
 * What a card shows about the machine's progress. Task 005's six badges plus
 * `interrupted`, which is a rendering of `failed` rather than a seventh state:
 * `run_state` has exactly ADR-0007's seven values and `interrupted` is not one
 * of them (seam-contract D9).
 */
export type CardBadge =
  | "running"
  | "queued"
  | "blocked"
  | "waiting_retry"
  | "failed"
  | "interrupted"
  | "cancelled";

/**
 * The badge a card shows, or `null` for `idle` — a task nothing has happened to
 * yet says nothing, rather than saying "idle".
 *
 * A function of three fields, not one. Seam-contract D9 accounts for the first
 * two: a run killed by a crash leaves `exit_class = 'interrupted'` on its `runs`
 * row and the task in `run_state = 'failed'`, and the word the user needs
 * ("interrupted", not a bare "failed" for something they did not do) lives on
 * the run. The exit class only ever refines `failed`, because a task that has
 * moved on to running or queued again is telling the user where it is now, not
 * why it stopped last time.
 *
 * `blockedByIncomplete` is ADR-0008's, and it is deliberately consulted **only
 * for `idle`**. `run_state` has a `blocked` value and ADR-0008's amendment of
 * 2026-09-02 leaves it unwritten — blocking is derived per read, and the state a
 * blocked card is actually in is `idle` — so this is where the two spellings
 * meet. It is last rather than first because every other state already says
 * something truer about right now: a *running* task whose dependency moved is
 * still running, and a *failed* one is still failed, which ADR-0007's failure
 * rule wants visible so it interrupts the morning review.
 */
export function cardBadge(
  runState: RunState,
  lastRun: { readonly exitClass: ExitClass | null } | null,
  blockedByIncomplete: boolean,
): CardBadge | null {
  switch (runState) {
    case "idle":
      return blockedByIncomplete ? "blocked" : null;
    case "failed":
      return lastRun?.exitClass === "interrupted" ? "interrupted" : "failed";
    case "running":
    case "queued":
    case "blocked":
    case "waiting_retry":
    case "cancelled":
      return runState;
  }
}

const MINUTE_MS = 60_000;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;
const YEAR_MS = 365 * DAY_MS;

/**
 * "Relative time of last activity" for a card, in the compact form a card has
 * room for: `just now` under a minute, then `5m ago`, `3h ago`, `2d ago`,
 * `1y ago`.
 *
 * `now` is a parameter because the alternative is `new Date()` inside a
 * component, which is a clock that cannot be faked and therefore a rendering
 * that cannot be tested (CLAUDE.md: fake the clock). A timestamp in the future
 * — a task written by a machine whose clock runs ahead — reads `just now`
 * rather than a negative count.
 */
export function relativeTime(timestamp: string, now: Date): string {
  const then = Date.parse(timestamp);
  if (Number.isNaN(then)) return "";

  const elapsed = now.getTime() - then;
  if (elapsed < MINUTE_MS) return "just now";
  if (elapsed < HOUR_MS) return `${Math.floor(elapsed / MINUTE_MS)}m ago`;
  if (elapsed < DAY_MS) return `${Math.floor(elapsed / HOUR_MS)}h ago`;
  if (elapsed < YEAR_MS) return `${Math.floor(elapsed / DAY_MS)}d ago`;
  return `${Math.floor(elapsed / YEAR_MS)}y ago`;
}
