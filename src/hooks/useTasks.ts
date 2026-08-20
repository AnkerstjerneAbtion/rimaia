import { useCallback, useEffect, useRef, useState } from "react";

import { listRepositories, listTasks, moveTask, toRimaiaError } from "../lib/commands";
import { subscribeToRepositoriesChanged, subscribeToTasksChanged } from "../lib/events";
import {
  boardReducer,
  initialBoardState,
  planMove,
  settlementReproducesMove,
  visibleColumns,
} from "../lib/board";
import type { BoardAction, BoardState } from "../lib/board";
import type {
  BoardColumn,
  RimaiaError,
  Repository,
  TaskFilterInput,
  TaskSummary,
} from "../types";

/**
 * The board's fetch/subscribe/optimistic-move wiring (task 005), sitting on
 * top of `lib/board.ts`'s pure reducer. A component owns none of the
 * optimistic-move policy itself — it only supplies `(taskId, column, index)`
 * and reads back `state`.
 *
 * `repositoryId` is a hook argument rather than a filter object so a change
 * to it is an explicit, memoisable dependency: switching the board's
 * repository filter is a fresh `list_tasks` read, the same as any other
 * filter change would be.
 */
export interface UseTasksResult {
  /**
   * The board's cards, as `list_tasks`'s summary projection returns them
   * (seam-contract D12) — every `tasks` column plus the link and dependency
   * counts and the last-run summary a card renders. The panel reads
   * `get_task` instead; this is the one read the whole board costs.
   */
  readonly state: BoardState<TaskSummary>;
  /** True until the first read (for this `repositoryId`) resolves. */
  readonly loading: boolean;
  readonly readError: RimaiaError | null;
  /**
   * Re-reads now, independent of any subscription — used after a mutation
   * this window itself caused (e.g. creating a task), the same "call it
   * directly rather than wait for the round trip" pattern
   * `RepositoriesSection` uses.
   *
   * Returns the read's own promise so a caller that needs the *result*
   * committed to `state` before acting further (selecting a just-created
   * task) can await it, rather than racing the optimistic path that reads
   * `state.tasks` on the very next render.
   */
  readonly refresh: () => Promise<void>;
  /**
   * Translates a drop and sends it. Wiring contract: `planMove` against the
   * board as currently displayed (server order plus every move still in
   * flight), `move_started` before the call so the card jumps immediately,
   * then `move_settled` with the row `move_task` returns or `move_rejected`
   * with its message — never a paraphrase of what the service actually said
   * (seam-contract D8 puts the specificity there). A settlement that the
   * merged row cannot account for on its own falls back to a full re-read;
   * see the call site.
   *
   * A `planMove` that returns `null` (no board change, or the id is gone)
   * sends nothing — there is no in-flight state to represent a call that was
   * never made.
   */
  readonly moveCard: (taskId: string, column: BoardColumn, index: number) => void;
  readonly dismissRejection: () => void;
}

export function useTasks(repositoryId: string | null): UseTasksResult {
  const [state, setState] = useState<BoardState<TaskSummary>>(() =>
    initialBoardState<TaskSummary>(),
  );
  const [loading, setLoading] = useState(true);
  const [readError, setReadError] = useState<RimaiaError | null>(null);

  // `moveCard` needs to plan against the board as currently displayed from a
  // plain (non-updater) function body — a `setState` updater must be pure,
  // and issuing the `moveTask` IPC call from inside one meant React's
  // StrictMode double-invocation of updaters in dev turned one drag into two
  // identical `move_task` requests. Kept in step with `state` on every
  // render, and advanced synchronously by `moveCard` itself so two moves
  // issued before React re-renders still see each other's `nextSeq`.
  const stateRef = useRef(state);
  stateRef.current = state;

  // Guards against a stale `list_tasks` response landing after a newer one
  // already has (switching `repositoryId` mid-flight, or two `tasks:changed`
  // re-reads overlapping) — only the response for the most recently issued
  // request is allowed to commit.
  const latestRequestId = useRef(0);

  const refresh = useCallback(() => {
    const filter: TaskFilterInput = repositoryId ? { repositoryId } : {};
    const requestId = (latestRequestId.current += 1);
    return listTasks(filter).then(
      (tasks) => {
        if (latestRequestId.current !== requestId) return;
        setState((prev) => boardReducer(prev, { kind: "tasks_read", tasks }));
        setReadError(null);
        setLoading(false);
      },
      (thrown) => {
        if (latestRequestId.current !== requestId) return;
        setReadError(toRimaiaError(thrown));
        setLoading(false);
      },
    );
  }, [repositoryId]);

  useEffect(() => {
    setLoading(true);
    refresh();
  }, [refresh]);

  useEffect(() => {
    // ADR-0018: every payload on `tasks:changed` means "re-read" - the
    // specific ids case and the shell forwarder's empty-array "re-read this
    // entity wholesale" recovery case alike. The board always displays the
    // whole (filtered) list, so there is no per-id fetch to reconcile a
    // partial payload against; a non-empty array gets the same full refresh
    // an empty one does; see `events.ts`'s own contract note.
    let active = true;
    let unlisten: (() => void) | undefined;
    subscribeToTasksChanged(() => {
      if (active) refresh();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview): live refresh is
        // unavailable, but `refresh()` above and every mutation below still
        // read fresh state on their own.
      },
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [refresh]);

  const moveCard = useCallback(
    (taskId: string, column: BoardColumn, index: number) => {
      const move = planMove(visibleColumns(stateRef.current), taskId, column, index);
      if (!move) return;

      const seq = stateRef.current.nextSeq;
      stateRef.current = boardReducer(stateRef.current, { kind: "move_started", seq, move });
      setState((current) => boardReducer(current, { kind: "move_started", seq, move }));

      moveTask(move.taskId, move.column, move.beforeId, move.afterId).then(
        (placed) => {
          const settled: BoardAction<TaskSummary> = { kind: "move_settled", seq, placed };
          setState((s) => boardReducer(s, settled));

          // `move_task` answers with one row, but it renumbers the whole
          // destination column whenever the gap between the drop's
          // neighbours has closed - and one post-rebalance number merged
          // into siblings holding pre-rebalance ones sorts that column by
          // two incompatible scales, which under ADR-0007 is a false claim
          // about what the queue runs next. There is no repair available
          // from a single row, so re-read; `refresh`'s generation guard is
          // what keeps this read from being clobbered by an older one
          // (including the `tasks:changed` re-read this same mutation
          // triggers). Checked against `stateRef` rather than the settled
          // React state because a `setState` updater must stay pure - a
          // ref one render behind can only ask for a read that was not
          // needed, never miss one that was.
          if (!settlementReproducesMove(boardReducer(stateRef.current, settled).tasks, move)) {
            refresh();
          }
        },
        (thrown) =>
          setState((s) =>
            boardReducer(s, {
              kind: "move_rejected",
              seq,
              reason: toRimaiaError(thrown).message,
            }),
          ),
      );
    },
    [refresh],
  );

  const dismissRejection = useCallback(() => {
    setState((s) => boardReducer(s, { kind: "rejection_dismissed" }));
  }, []);

  return { state, loading, readError, refresh, moveCard, dismissRejection };
}

export interface UseRepositoriesResult {
  readonly repositories: readonly Repository[];
  readonly loading: boolean;
  readonly error: RimaiaError | null;
}

/**
 * Colocated with `useTasks` rather than split into its own hook file: the
 * board needs repository names for cards and the filter dropdown, this
 * stage's file list names exactly one new hook file, and the fetch/subscribe
 * shape is identical to `RepositoriesSection`'s (a live list, refreshed on
 * `repositories:changed`) - there is nothing board-specific about it.
 */
export function useRepositories(): UseRepositoriesResult {
  const [repositories, setRepositories] = useState<Repository[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<RimaiaError | null>(null);

  const refresh = useCallback(() => {
    listRepositories().then(
      (repos) => {
        setRepositories([...repos].sort((a, b) => a.name.localeCompare(b.name)));
        setError(null);
        setLoading(false);
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setLoading(false);
      },
    );
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;
    subscribeToRepositoriesChanged(() => {
      if (active) refresh();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge - same fallback as useTasks above.
      },
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [refresh]);

  return { repositories, loading, error };
}
