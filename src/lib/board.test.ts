import { describe, expect, it } from "vitest";

import type { BoardColumn, ExitClass, RunState, Task } from "../types";

import {
  BOARD_COLUMNS,
  boardReducer,
  cardBadge,
  formatResumeAfter,
  groupIntoColumns,
  initialBoardState,
  planMove,
  relativeTime,
  settlementReproducesMove,
  visibleColumns,
} from "./board";
import type { BoardCard, BoardColumns, BoardState, PlannedMove } from "./board";

const REPO_A = "repo-a";
const REPO_B = "repo-b";

/** A real `Task`, so every test below also proves `Task` satisfies `BoardCard`
 *  structurally — the reason the module's types are minimal rather than a
 *  second mirror of the Rust DTO. */
const BASE_TASK: Task = {
  id: "base",
  repositoryId: REPO_A,
  title: "base",
  plan: "a plan",
  extraInstructions: null,
  column: "ready",
  position: 0,
  runState: "idle",
  branch: null,
  worktreePath: null,
  strategyMode: "default",
  model: null,
  effort: null,
  strategyPlan: null,
  strategySource: null,
  strategyUpdatedAt: null,
  createdAt: "2026-08-20T09:00:00Z",
  updatedAt: "2026-08-20T09:00:00Z",
  source: "ui",
};

function card(id: string, overrides: Partial<Task> = {}): Task {
  return { ...BASE_TASK, id, title: id, ...overrides };
}

/** A column whose displayed order is exactly `ids`, spaced the way a freshly
 *  rebalanced column is. */
function laneOf(column: BoardColumn, ids: readonly string[], repositoryId = REPO_A): Task[] {
  return ids.map((id, index) => card(id, { column, repositoryId, position: index }));
}

function ids<T extends BoardCard>(cards: readonly T[]): string[] {
  return cards.map((entry) => entry.id);
}

/** Two repositories' cards in one `ready` column: repo-a's block, then
 *  repo-b's, which is the order `groupIntoColumns` produces for them. */
const MIXED_READY: Task[] = [
  card("a1", { column: "ready", repositoryId: REPO_A, position: 0 }),
  card("a2", { column: "ready", repositoryId: REPO_A, position: 1 }),
  card("b1", { column: "ready", repositoryId: REPO_B, position: 0 }),
  card("b2", { column: "ready", repositoryId: REPO_B, position: 1 }),
];

/** What a component does: plan against what the user can see, not against the
 *  last server read. */
function plan<T extends BoardCard>(
  state: BoardState<T>,
  taskId: string,
  column: BoardColumn,
  index: number,
): PlannedMove {
  const move = planMove(visibleColumns(state), taskId, column, index);
  if (!move) throw new Error(`expected a move for ${taskId} into ${column}[${index}]`);
  return move;
}

function start<T extends BoardCard>(state: BoardState<T>, move: PlannedMove): BoardState<T> {
  return boardReducer(state, { kind: "move_started", seq: state.nextSeq, move });
}

describe("BOARD_COLUMNS", () => {
  it("lists ADR-0007's four columns in board order", () => {
    expect(BOARD_COLUMNS).toEqual(["not_ready", "ready", "in_review", "done"]);
  });
});

describe("groupIntoColumns", () => {
  it("returns all four columns for an empty board", () => {
    expect(groupIntoColumns([])).toEqual({
      not_ready: [],
      ready: [],
      in_review: [],
      done: [],
    });
  });

  it("places every card in the column its row names", () => {
    const columns = groupIntoColumns([
      card("n", { column: "not_ready" }),
      card("r", { column: "ready" }),
      card("v", { column: "in_review" }),
      card("d", { column: "done" }),
    ]);

    expect(ids(columns.not_ready)).toEqual(["n"]);
    expect(ids(columns.ready)).toEqual(["r"]);
    expect(ids(columns.in_review)).toEqual(["v"]);
    expect(ids(columns.done)).toEqual(["d"]);
  });

  it("orders a column by ascending position", () => {
    const columns = groupIntoColumns([
      card("third", { position: 2.5 }),
      card("first", { position: -1 }),
      card("second", { position: 0 }),
    ]);

    expect(ids(columns.ready)).toEqual(["first", "second", "third"]);
  });

  it("breaks a shared position on createdAt, then on id", () => {
    // The degenerate case `rebalance_column` exists to repair: nothing makes
    // `position` unique, so the board has to be deterministic without it.
    const columns = groupIntoColumns([
      card("zz", { position: 1, createdAt: "2026-08-20T10:00:00Z" }),
      card("aa", { position: 1, createdAt: "2026-08-20T10:00:00Z" }),
      card("older", { position: 1, createdAt: "2026-08-19T10:00:00Z" }),
    ]);

    expect(ids(columns.ready)).toEqual(["older", "aa", "zz"]);
  });

  it("keeps each repository's cards contiguous when the board shows every repository", () => {
    // repo-b's 0.5 sits between repo-a's 0 and 1, but the two numbers are not
    // comparable: position is only meaningful within one (repository, column).
    const columns = groupIntoColumns([
      card("a-low", { repositoryId: REPO_A, position: 0 }),
      card("b-mid", { repositoryId: REPO_B, position: 0.5 }),
      card("a-high", { repositoryId: REPO_A, position: 1 }),
    ]);

    expect(ids(columns.ready)).toEqual(["a-low", "a-high", "b-mid"]);
  });

  it("drops a card whose column is none of the four rather than throwing", () => {
    const columns = groupIntoColumns([
      card("kept"),
      card("hand-edited", { column: "archived" as unknown as BoardColumn }),
    ]);

    expect(ids(columns.ready)).toEqual(["kept"]);
    expect(ids(columns.not_ready)).toEqual([]);
  });
});

describe("planMove", () => {
  const lane = (): BoardColumns<Task> => groupIntoColumns(laneOf("ready", ["a", "b", "c", "d"]));

  it("returns null for a card the board does not hold", () => {
    expect(planMove(lane(), "missing", "ready", 0)).toBeNull();
  });

  it("names no before neighbour for a drop at the top of a column", () => {
    expect(planMove(lane(), "d", "ready", 0)).toEqual({
      taskId: "d",
      column: "ready",
      beforeId: null,
      afterId: "a",
      displayIndex: 0,
    });
  });

  it("names no after neighbour for a drop at the bottom of a column", () => {
    expect(planMove(lane(), "a", "ready", 3)).toEqual({
      taskId: "a",
      column: "ready",
      beforeId: "d",
      afterId: null,
      displayIndex: 3,
    });
  });

  it("names both neighbours for a drop between two cards", () => {
    expect(planMove(lane(), "d", "ready", 1)).toEqual({
      taskId: "d",
      column: "ready",
      beforeId: "a",
      afterId: "b",
      displayIndex: 1,
    });
  });

  it("names neither neighbour for a drop into an empty column", () => {
    // `move_task` accepts a naked pair only here, and refuses it anywhere else.
    expect(planMove(lane(), "a", "done", 0)).toEqual({
      taskId: "a",
      column: "done",
      beforeId: null,
      afterId: null,
      displayIndex: 0,
    });
  });

  it("reads neighbours from the destination column when a card crosses columns", () => {
    const columns = groupIntoColumns([
      ...laneOf("ready", ["a", "b"]),
      ...laneOf("in_review", ["x", "y"]),
    ]);

    expect(planMove(columns, "a", "in_review", 1)).toMatchObject({
      column: "in_review",
      beforeId: "x",
      afterId: "y",
    });
  });

  it("returns null when a card is dropped back onto its own slot", () => {
    const columns = lane();

    ["a", "b", "c", "d"].forEach((id, index) => {
      expect(planMove(columns, id, "ready", index)).toBeNull();
    });
  });

  it("names the card it passed when a card moves one slot down its own column", () => {
    // The off-by-one: `b` leaving shifts `c` and `d` up one, so an index into
    // the column as it looked before the drag names `b` itself as the
    // neighbour, which `move_task` refuses outright.
    expect(planMove(lane(), "b", "ready", 2)).toEqual({
      taskId: "b",
      column: "ready",
      beforeId: "c",
      afterId: "d",
      displayIndex: 2,
    });
  });

  it("names the card it passed when a card moves one slot up its own column", () => {
    expect(planMove(lane(), "c", "ready", 1)).toEqual({
      taskId: "c",
      column: "ready",
      beforeId: "a",
      afterId: "b",
      displayIndex: 1,
    });
  });

  it("never names the dragged card as its own neighbour, at any slot", () => {
    const columns = lane();

    for (const id of ["a", "b", "c", "d"]) {
      for (let index = 0; index <= 3; index += 1) {
        const move = planMove(columns, id, "ready", index);
        if (!move) continue;
        expect(move.beforeId, `before at ${index}`).not.toBe(id);
        expect(move.afterId, `after at ${index}`).not.toBe(id);
      }
    }
  });

  it("clamps an index past the end of the column to the bottom", () => {
    expect(planMove(lane(), "a", "ready", 99)).toMatchObject({
      beforeId: "d",
      afterId: null,
      displayIndex: 3,
    });
  });

  it("clamps a negative index to the top", () => {
    expect(planMove(lane(), "d", "ready", -4)).toMatchObject({
      beforeId: null,
      afterId: "a",
      displayIndex: 0,
    });
  });

  it("skips another repository's cards when choosing neighbours", () => {
    // `move_task` looks a neighbour up by (id, repository_id, board_column) and
    // refuses one from another repository, so naming `b2` here would be an
    // illegal call rather than a differently-ordered board.
    const columns = groupIntoColumns(MIXED_READY);

    expect(planMove(columns, "a1", "ready", 3)).toEqual({
      taskId: "a1",
      column: "ready",
      beforeId: "a2",
      afterId: null,
      displayIndex: 3,
    });
  });

  it("moves a card within its own repository's block on a mixed board", () => {
    const columns = groupIntoColumns(MIXED_READY);

    expect(planMove(columns, "b2", "ready", 0)).toEqual({
      taskId: "b2",
      column: "ready",
      beforeId: null,
      afterId: "b1",
      displayIndex: 0,
    });
  });

  it("returns null when a drop only changes which other repository's cards a card sits beside", () => {
    // `b1` is already the top of repo-b's block; dropping it above repo-a's
    // cards changes no order the backend can express, so nothing is sent.
    const columns = groupIntoColumns(MIXED_READY);

    expect(planMove(columns, "b1", "ready", 0)).toBeNull();
    expect(planMove(columns, "a2", "ready", 2)).toBeNull();
  });
});

describe("boardReducer", () => {
  const seeded = () => initialBoardState(laneOf("ready", ["a", "b", "c"]));

  it("shows the server's order when nothing is pending", () => {
    expect(ids(visibleColumns(seeded()).ready)).toEqual(["a", "b", "c"]);
  });

  it("shows the card at the slot it was dropped in before the service answers", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));

    expect(ids(visibleColumns(moved).ready)).toEqual(["b", "c", "a"]);
  });

  it("moves the card into its new column optimistically", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "done", 0));
    const columns = visibleColumns(moved);

    expect(ids(columns.ready)).toEqual(["b", "c"]);
    expect(ids(columns.done)).toEqual(["a"]);
    expect(columns.done[0].column).toBe("done");
  });

  it("keeps the optimistic order when a tasks:changed re-read arrives mid-flight", () => {
    // The clobber-and-clobber-back bug: `tasks:changed` fires for every
    // mutation, so a re-read routinely lands before this board's own move has
    // committed. The overlay is re-applied on top of whatever was read.
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));

    const reread = boardReducer(moved, {
      kind: "tasks_read",
      tasks: laneOf("ready", ["a", "b", "c"]),
    });

    expect(reread.pending).toHaveLength(1);
    expect(ids(visibleColumns(reread).ready)).toEqual(["b", "c", "a"]);
  });

  it("places a pending move by the neighbours it named, not by a drop index a concurrent insert has shifted", () => {
    // `a` was dropped below `c`. Another window then lands `fresh` between `b`
    // and `c`: an overlay that reinserted at the frozen drop index would show
    // `a` above `c`, which is not the move that was sent.
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));
    expect(moved.pending[0].move).toMatchObject({ beforeId: "c", afterId: null });

    const reread = boardReducer(moved, {
      kind: "tasks_read",
      tasks: [
        card("a", { position: 0 }),
        card("b", { position: 1 }),
        card("fresh", { position: 1.5 }),
        card("c", { position: 2 }),
      ],
    });

    expect(ids(visibleColumns(reread).ready)).toEqual(["b", "fresh", "c", "a"]);
  });

  it("applies two in-flight moves in the order they were issued", () => {
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));

    expect(second.pending.map((entry) => entry.seq)).toEqual([1, 2]);
    expect(ids(visibleColumns(second).ready)).toEqual(["c", "b", "a"]);
  });

  it("keeps a later move's overlay when an earlier one settles", () => {
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));

    const settled = boardReducer(second, {
      kind: "move_settled",
      seq: 1,
      placed: { id: "a", column: "ready", position: 3, updatedAt: "2026-08-20T11:00:00Z" },
    });

    expect(settled.pending.map((entry) => entry.seq)).toEqual([2]);
    expect(ids(visibleColumns(settled).ready)).toEqual(["c", "b", "a"]);
  });

  it("takes the column and position back from the settled row", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "done", 0));

    const settled = boardReducer(moved, {
      kind: "move_settled",
      seq: 1,
      placed: { id: "a", column: "done", position: 7.5, updatedAt: "2026-08-20T11:00:00Z" },
    });

    expect(settled.pending).toEqual([]);
    expect(ids(visibleColumns(settled).done)).toEqual(["a"]);
    expect(settled.tasks.find((task) => task.id === "a")).toMatchObject({
      column: "done",
      position: 7.5,
      updatedAt: "2026-08-20T11:00:00Z",
      title: "a",
      plan: "a plan",
    });
  });

  it("snaps a rejected move back to the server's order", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));

    const rejected = boardReducer(moved, {
      kind: "move_rejected",
      seq: 1,
      reason: 'cannot put "a" in ready without a plan',
    });

    expect(rejected.pending).toEqual([]);
    expect(ids(visibleColumns(rejected).ready)).toEqual(["a", "b", "c"]);
    expect(rejected.rejection).toEqual({
      taskId: "a",
      reason: 'cannot put "a" in ready without a plan',
    });
  });

  it("discards moves issued after a rejected one", () => {
    // The later drop was planned against an arrangement the server never had,
    // so its neighbour ids describe an order that does not exist.
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));

    const rejected = boardReducer(second, {
      kind: "move_rejected",
      seq: 1,
      reason: "refused",
    });

    expect(rejected.pending).toEqual([]);
    expect(ids(visibleColumns(rejected).ready)).toEqual(["a", "b", "c"]);
  });

  it("ignores a rejection for a move an earlier rejection already discarded", () => {
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));
    const rejected = boardReducer(second, { kind: "move_rejected", seq: 1, reason: "first" });

    const late = boardReducer(rejected, { kind: "move_rejected", seq: 2, reason: "second" });

    expect(late).toBe(rejected);
    expect(late.rejection?.reason).toBe("first");
  });

  it("merges a settled placement even for a move a rejection discarded", () => {
    // Discarding the overlay does not un-send the call: if the service
    // committed it anyway, the row it returns is still truth.
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));
    const rejected = boardReducer(second, { kind: "move_rejected", seq: 1, reason: "refused" });

    const settled = boardReducer(rejected, {
      kind: "move_settled",
      seq: 2,
      placed: { id: "c", column: "ready", position: -1, updatedAt: "2026-08-20T11:00:00Z" },
    });

    expect(ids(visibleColumns(settled).ready)).toEqual(["c", "a", "b"]);
  });

  it("leaves the board unchanged when a pending move's card was deleted elsewhere", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));

    const reread = boardReducer(moved, {
      kind: "tasks_read",
      tasks: laneOf("ready", ["b", "c"]),
    });

    expect(ids(visibleColumns(reread).ready)).toEqual(["b", "c"]);
  });

  it("clears the rejection when another move starts", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));
    const rejected = boardReducer(moved, { kind: "move_rejected", seq: 1, reason: "refused" });

    expect(start(rejected, plan(rejected, "a", "ready", 2)).rejection).toBeNull();
  });

  it("clears the rejection when it is dismissed", () => {
    const state = seeded();
    const moved = start(state, plan(state, "a", "ready", 2));
    const rejected = boardReducer(moved, { kind: "move_rejected", seq: 1, reason: "refused" });

    expect(boardReducer(rejected, { kind: "rejection_dismissed" }).rejection).toBeNull();
  });

  it("hands out a fresh seq for every move", () => {
    const state = seeded();
    const first = start(state, plan(state, "a", "ready", 2));
    const second = start(first, plan(first, "c", "ready", 0));

    expect([state.nextSeq, first.nextSeq, second.nextSeq]).toEqual([1, 2, 3]);
  });
});

describe("settlementReproducesMove", () => {
  it("returns false when a rebalance renumbered the column and the merge only touched the moved row", () => {
    // `move_task` answers with one row, but closing the gap between two
    // neighbours forces `position::rebalance_column` (seam-contract D1) to
    // renumber the whole column. `a` and `b` here sit close enough together
    // that dropping `z` between them is exactly that case — the merge only
    // updates `z`, so `a` and `b` keep the stale positions they had before
    // the rebalance, and `z` (merged at the post-rebalance midpoint) sorts
    // *above* both of them instead of between them.
    const tasks = [
      card("z", { column: "not_ready", position: 0 }),
      card("a", { column: "ready", position: 1 }),
      card("b", { column: "ready", position: 1.0000001 }),
    ];
    const move: PlannedMove = {
      taskId: "z",
      column: "ready",
      beforeId: "a",
      afterId: "b",
      displayIndex: 1,
    };
    const merged = tasks.map((task) =>
      task.id === "z" ? { ...task, column: "ready" as const, position: 0.5 } : task,
    );

    expect(ids(groupIntoColumns(merged).ready)).toEqual(["z", "a", "b"]);
    expect(settlementReproducesMove(merged, move)).toBe(false);
  });

  it("returns true when the settled row's neighbours match what the drop asked for", () => {
    const tasks = [
      card("z", { column: "not_ready", position: 0 }),
      card("a", { column: "ready", position: 0 }),
      card("b", { column: "ready", position: 1 }),
    ];
    const move: PlannedMove = {
      taskId: "z",
      column: "ready",
      beforeId: "a",
      afterId: "b",
      displayIndex: 1,
    };
    const merged = tasks.map((task) =>
      task.id === "z" ? { ...task, column: "ready" as const, position: 0.5 } : task,
    );

    expect(ids(groupIntoColumns(merged).ready)).toEqual(["a", "z", "b"]);
    expect(settlementReproducesMove(merged, move)).toBe(true);
  });

  it("returns false when the moved card is gone from the settled tasks (deleted, or moved elsewhere by another window)", () => {
    const move: PlannedMove = {
      taskId: "ghost",
      column: "ready",
      beforeId: null,
      afterId: null,
      displayIndex: 0,
    };

    expect(settlementReproducesMove(laneOf("ready", ["a", "b"]), move)).toBe(false);
  });
});

describe("cardBadge", () => {
  it("shows nothing for an idle task", () => {
    expect(cardBadge("idle", null, false)).toBeNull();
  });

  it("shows the run state itself for every state but failed", () => {
    const states: RunState[] = ["running", "queued", "blocked", "waiting_retry", "cancelled"];

    for (const state of states) {
      expect(cardBadge(state, null, false), state).toBe(state);
    }
  });

  it("reads interrupted off the last run's exit class, not off the run state", () => {
    // Seam-contract D9: `run_state` has no `interrupted`. A run that died with
    // the app leaves the task `failed` and the word on the run row.
    expect(cardBadge("failed", { exitClass: "interrupted" }, false)).toBe("interrupted");
  });

  it("shows failed when the last run stopped for any other reason", () => {
    const classes: ExitClass[] = ["fatal", "transient", "usage_limit", "cancelled", "success"];

    for (const exitClass of classes) {
      expect(cardBadge("failed", { exitClass }, false), exitClass).toBe("failed");
    }
  });

  it("shows failed when no run has been recorded yet", () => {
    expect(cardBadge("failed", null, false)).toBe("failed");
    expect(cardBadge("failed", { exitClass: null }, false)).toBe("failed");
  });

  it("ignores an interrupted last run once the task is queued again", () => {
    expect(cardBadge("queued", { exitClass: "interrupted" }, false)).toBe("queued");
    expect(cardBadge("idle", { exitClass: "interrupted" }, false)).toBeNull();
  });

  it("shows blocked for an idle task whose dependency is not satisfied", () => {
    // ADR-0008, and the one place `run_state`'s `blocked` value and the derived
    // `blocked_by_incomplete` flag meet: the state on the row is `idle`,
    // because the amendment of 2026-09-02 leaves `blocked` unwritten.
    expect(cardBadge("idle", null, true)).toBe("blocked");
  });

  it("does not let a blocked dependency overwrite what a card is doing now", () => {
    // A *running* task whose dependency moved is still running, and a *failed*
    // one is still failed — ADR-0007's failure rule wants that visible so it
    // interrupts the morning review rather than hiding behind "blocked".
    expect(cardBadge("running", null, true)).toBe("running");
    expect(cardBadge("queued", null, true)).toBe("queued");
    expect(cardBadge("waiting_retry", null, true)).toBe("waiting_retry");
    expect(cardBadge("cancelled", null, true)).toBe("cancelled");
    expect(cardBadge("failed", null, true)).toBe("failed");
    expect(cardBadge("failed", { exitClass: "interrupted" }, true)).toBe("interrupted");
  });
});

describe("relativeTime", () => {
  const now = new Date("2026-08-20T12:00:00Z");

  it("reads just now under a minute", () => {
    expect(relativeTime("2026-08-20T11:59:01Z", now)).toBe("just now");
  });

  it("counts whole minutes, hours and days", () => {
    expect(relativeTime("2026-08-20T11:55:00Z", now)).toBe("5m ago");
    expect(relativeTime("2026-08-20T08:30:00Z", now)).toBe("3h ago");
    expect(relativeTime("2026-08-18T13:00:00Z", now)).toBe("1d ago");
  });

  it("counts whole years past a year", () => {
    expect(relativeTime("2024-08-20T12:00:00Z", now)).toBe("2y ago");
  });

  it("reads just now for a timestamp in the future", () => {
    expect(relativeTime("2026-08-20T12:05:00Z", now)).toBe("just now");
  });

  it("returns nothing for an unparseable timestamp", () => {
    expect(relativeTime("not a date", now)).toBe("");
  });
});

describe("formatResumeAfter", () => {
  const now = new Date("2026-08-20T02:00:00Z");

  it("names a clock time rather than an interval", () => {
    // Deliberately unlike `relativeTime`, which this file's other formatter is.
    // A card is not re-rendered while it waits, so "in 4 hours" would be a lie
    // by 06:00 while "resumes 06:12" stays true whenever the board was drawn.
    const label = formatResumeAfter("2026-08-20T06:12:00Z", now);
    expect(label).toMatch(/^resumes /);
    expect(label).not.toContain("in ");
  });

  it("names the day when the wait crosses one", () => {
    // A bare "06:12" on a card whose window reopens tomorrow morning would read
    // as twenty minutes away.
    const label = formatResumeAfter("2026-08-22T06:12:00Z", now) ?? "";
    expect(label.length).toBeGreaterThan("resumes 06:12".length);
  });

  it("reads a deadline already past as resuming, not as a time gone by", () => {
    // What it means: the queue is due to pick the card up and has not got to it
    // yet — or is paused, which is what a launch after a crash looks like.
    expect(formatResumeAfter("2026-08-20T01:00:00Z", now)).toBe("resuming");
  });

  it("has nothing to say for a task with no deadline", () => {
    expect(formatResumeAfter(null, now)).toBeNull();
    expect(formatResumeAfter(undefined, now)).toBeNull();
    expect(formatResumeAfter("not a date", now)).toBeNull();
  });
});
