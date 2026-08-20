import { createElement } from "react";
import { act, fireEvent, render, renderHook, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  Board,
  describeDragEntity,
  findCard,
  isEditableTarget,
  nextFocusTarget,
  resolveDrop,
} from "./Board";
import { COLUMN_TITLES } from "./Column";
import { useTasks } from "../../hooks/useTasks";
import { groupIntoColumns } from "../../lib/board";
import type { Repository, Task } from "../../types";

// `commands.ts` and `events.ts` both bottom out in `@tauri-apps/api/core` and
// `@tauri-apps/api/event` respectively - mocking here, not the wrappers,
// exercises the real `toRimaiaError` path exactly as
// `StorageSection.test.tsx`'s own comment explains.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

// dnd-kit's own drag choreography cannot be driven from jsdom - pointer and
// keyboard sensors alike depend on real element rects, which jsdom reports
// as all zero (see the comment above the `resolveDrop`/`nextFocusTarget`
// describe blocks below). The one test that needs `Board`'s own `onDragEnd`
// to actually run captures the callback dnd-kit itself would call and
// invokes it directly with a synthetic event - bypassing dnd-kit's own
// (untestable) collision detection rather than pretending to simulate it.
// Every other export of `@dnd-kit/core` (`useDroppable`, sensors, and so on)
// stays real, so every other test's rendering is unaffected.
const dndContextSpy = vi.hoisted(() => ({
  onDragEnd: null as ((event: unknown) => void) | null,
}));

vi.mock("@dnd-kit/core", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@dnd-kit/core")>();
  return {
    ...actual,
    DndContext: (props: Record<string, unknown>) => {
      dndContextSpy.onDragEnd = (props.onDragEnd as (event: unknown) => void) ?? null;
      return createElement(actual.DndContext, props as never);
    },
  };
});

/** A minimal `DragEndEvent` - just enough of `active`/`over` for
 *  `handleDragEnd` to read, standing in for the geometry dnd-kit itself
 *  would supply and that jsdom cannot. */
function fakeDragEndEvent(activeId: string, overId: string) {
  return {
    active: { id: activeId, rect: { current: { translated: null } } },
    over: { id: overId, rect: { top: 0, left: 0, width: 0, height: 0 } },
  };
}

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "t1",
    repositoryId: "repo-1",
    title: "Wire the board",
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
    createdAt: "2026-08-20T11:00:00Z",
    updatedAt: "2026-08-20T11:00:00Z",
    ...overrides,
  };
}

function repository(overrides: Partial<Repository> = {}): Repository {
  return {
    id: "repo-1",
    name: "rimaia",
    path: "/code/rimaia",
    defaultBranch: "main",
    worktreeRoot: "/data/worktrees/rimaia",
    allowUnattendedRuns: false,
    createdAt: "2026-08-20T09:00:00Z",
    ...overrides,
  };
}

/** Every test's default backend: a fixed repository list, a fixed task list,
 *  and every `listen` subscription resolves (captured for the tests that
 *  drive it) and never fires on its own. */
function mockBackend({
  tasks = [] as Task[],
  repositories = [repository()],
}: { tasks?: Task[]; repositories?: Repository[] } = {}) {
  const listenHandlers: Record<string, (event: { payload: unknown }) => void> = {};
  const unlistenSpies: Record<string, ReturnType<typeof vi.fn>> = {};

  mockListen.mockImplementation(async (eventName, callback) => {
    listenHandlers[eventName as string] = callback as (event: { payload: unknown }) => void;
    const unlisten = vi.fn();
    unlistenSpies[eventName as string] = unlisten;
    return unlisten;
  });

  mockInvoke.mockImplementation(async (command) => {
    if (command === "list_tasks") return tasks;
    if (command === "list_repositories") return repositories;
    throw new Error(`unexpected command: ${command}`);
  });

  return { listenHandlers, unlistenSpies };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
});

// ---------------------------------------------------------------------------
// Pure helpers — the drop translation and the arrow-key navigation target.
// Called directly with plain data, never through a simulated drag: jsdom has
// no layout engine, so a "drag" there would prove nothing (see the board
// agent's own note on this).
// ---------------------------------------------------------------------------

describe("resolveDrop", () => {
  const columns = groupIntoColumns([
    task({ id: "a", column: "ready", position: 0 }),
    task({ id: "b", column: "ready", position: 1 }),
    task({ id: "x", column: "in_review", position: 0 }),
  ]);

  it("targets the end of the column when dropped on the column's own droppable id", () => {
    expect(resolveDrop(columns, "a", "ready", false)).toEqual({ column: "ready", index: 2 });
  });

  it("targets a card's own index in its column when dropped over it, ignoring the midpoint flag within the same column", () => {
    // Same-column: the pre-removal index already names the right neighbour
    // once the dragged card's own self-removal shift is accounted for
    // (`planMove`'s doc comment) — the midpoint flag must not double-count it.
    expect(resolveDrop(columns, "a", "b", false)).toEqual({ column: "ready", index: 1 });
    expect(resolveDrop(columns, "a", "b", true)).toEqual({ column: "ready", index: 1 });
  });

  it("returns null for an id that names neither a column nor a known card", () => {
    expect(resolveDrop(columns, "a", "ghost", false)).toBeNull();
  });

  it("coerces a numeric UniqueIdentifier the same way a string one is handled", () => {
    // dnd-kit's `UniqueIdentifier` is `string | number`; every id this board
    // hands it is a string (seam-contract D10), but the coercion has to hold
    // for whatever dnd-kit itself passes back.
    expect(resolveDrop(columns, "a", 404, false)).toBeNull();
  });

  it("lands above a card in a different column when dropped in its top half", () => {
    expect(resolveDrop(columns, "x", "a", false)).toEqual({ column: "ready", index: 0 });
  });

  it("lands below a card in a different column when dropped past its midpoint — otherwise the only way to reach a column's bottom slot is its own empty area", () => {
    expect(resolveDrop(columns, "x", "b", true)).toEqual({ column: "ready", index: 2 });
  });

  it("never adjusts for a card dropped over itself, or over an id the board holds nowhere else", () => {
    // `activeColumn` is undefined for an id the board does not hold, so the
    // cross-column check is vacuously false rather than throwing.
    expect(resolveDrop(columns, "ghost", "a", true)).toEqual({ column: "ready", index: 0 });
  });
});

describe("describeDragEntity", () => {
  const columns = groupIntoColumns([task({ id: "a", title: "Wire the board", column: "ready" })]);

  it("resolves a card id to its title", () => {
    expect(describeDragEntity(columns, "a")).toBe("Wire the board");
  });

  it("resolves a column id to its display name", () => {
    expect(describeDragEntity(columns, "ready")).toBe(COLUMN_TITLES.ready);
  });

  it("falls back to the raw id when nothing matches, rather than throwing", () => {
    expect(describeDragEntity(columns, "ghost")).toBe("ghost");
  });
});

describe("nextFocusTarget", () => {
  const columns = groupIntoColumns([
    task({ id: "a", column: "ready", position: 0 }),
    task({ id: "b", column: "ready", position: 1 }),
    task({ id: "x", column: "in_review", position: 0 }),
  ]);

  it("moves up and down within a column", () => {
    expect(nextFocusTarget(columns, "b", "ArrowUp")).toBe("a");
    expect(nextFocusTarget(columns, "a", "ArrowDown")).toBe("b");
  });

  it("returns null at the top and bottom of a column", () => {
    expect(nextFocusTarget(columns, "a", "ArrowUp")).toBeNull();
    expect(nextFocusTarget(columns, "b", "ArrowDown")).toBeNull();
  });

  it("crosses to the adjacent column at the same row, clamped to its length", () => {
    expect(nextFocusTarget(columns, "b", "ArrowRight")).toBe("x"); // in_review has one card
    expect(nextFocusTarget(columns, "x", "ArrowLeft")).toBe("a");
  });

  it("returns null crossing into a column with no cards", () => {
    expect(nextFocusTarget(columns, "x", "ArrowRight")).toBeNull(); // done is empty
  });

  it("returns null for a card the board does not hold", () => {
    expect(nextFocusTarget(columns, "ghost", "ArrowDown")).toBeNull();
  });
});

describe("isEditableTarget", () => {
  it("treats inputs, textareas and contenteditable elements as typing surfaces", () => {
    expect(isEditableTarget(document.createElement("input"))).toBe(true);
    expect(isEditableTarget(document.createElement("textarea"))).toBe(true);
    // jsdom implements neither `contentEditable`'s setter nor
    // `isContentEditable` at all (a documented jsdom gap) - the attribute is
    // what `isEditableTarget` falls back to, and what a test can set.
    const editable = document.createElement("div");
    editable.setAttribute("contenteditable", "true");
    expect(isEditableTarget(editable)).toBe(true);
  });

  it("does not treat a plain element, or null, as a typing surface", () => {
    expect(isEditableTarget(document.createElement("div"))).toBe(false);
    expect(isEditableTarget(null)).toBe(false);
  });
});

describe("findCard", () => {
  it("finds a card across every column, and returns null when it holds none or is asked for none", () => {
    const columns = groupIntoColumns([task({ id: "a", column: "done" })]);
    expect(findCard(columns, "a")?.id).toBe("a");
    expect(findCard(columns, "missing")).toBeNull();
    expect(findCard(columns, null)).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// The drop-translation-to-send pipeline (`useTasks.moveCard`) — the part of
// "a drop was translated" that is provable without a simulated drag: given a
// translated `(column, index)`, is the right command sent with the right
// arguments, and does a rejection carry the backend's own reason. `Board`'s
// `handleDragEnd` is three lines of glue over `resolveDrop` (proved above)
// and this; there is nothing left in it that a simulated pointer/keyboard
// drag would prove and this does not, and jsdom cannot drive one reliably
// (dnd-kit's keyboard coordinate getter compares droppable rects, which are
// all-zero under jsdom, so it never finds a container to move into either).
// ---------------------------------------------------------------------------

describe("useTasks: drop translation and rejection", () => {
  it("sends the translated move_task call and merges the row it returns", async () => {
    const tasks = [task({ id: "a", position: 0 }), task({ id: "b", position: 1 })];
    mockBackend({ tasks });

    const { result } = renderHook(() => useTasks(null));
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(2));

    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_tasks") return tasks;
      if (command === "move_task") {
        expect(args).toEqual({ id: "a", column: "ready", beforeId: "b", afterId: null });
        return { ...tasks[0], position: 5, updatedAt: "2026-08-20T12:00:00Z" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    act(() => {
      result.current.moveCard("a", "ready", 2); // drop below b
    });

    await waitFor(() => expect(result.current.state.pending).toHaveLength(0));
    expect(result.current.state.tasks.find((t) => t.id === "a")).toMatchObject({ position: 5 });
  });

  it("carries the backend's own rejection reason, not a generic one", async () => {
    const tasks = [task({ id: "a", position: 0 }), task({ id: "b", position: 1 })];
    mockBackend({ tasks });

    const { result } = renderHook(() => useTasks(null));
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(2));

    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") return tasks;
      if (command === "move_task") {
        throw { code: "invalid", message: 'cannot put "a" in ready without a plan' };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    act(() => {
      result.current.moveCard("a", "ready", 2);
    });

    await waitFor(() =>
      expect(result.current.state.rejection).toEqual({
        taskId: "a",
        reason: 'cannot put "a" in ready without a plan',
      }),
    );
  });

  it("sends nothing for a drop that changes no order", async () => {
    const tasks = [task({ id: "a", position: 0 })];
    mockBackend({ tasks });
    const { result } = renderHook(() => useTasks(null));
    await waitFor(() => expect(result.current.state.tasks).toHaveLength(1));

    const callsBefore = mockInvoke.mock.calls.length;
    act(() => {
      result.current.moveCard("a", "ready", 0); // already there
    });

    expect(mockInvoke.mock.calls.length).toBe(callsBefore);
  });
});

// ---------------------------------------------------------------------------
// The board component itself.
// ---------------------------------------------------------------------------

describe("Board", () => {
  it("renders tasks grouped into their columns, with the repository name resolved", async () => {
    mockBackend({
      tasks: [
        task({ id: "a", column: "not_ready", title: "Write the ADR" }),
        task({ id: "b", column: "ready", title: "Wire the board" }),
      ],
    });

    render(<Board />);

    expect(await screen.findByText("Write the ADR")).toBeInTheDocument();
    expect(screen.getByText("Wire the board")).toBeInTheDocument();
    // Scoped to the card's own repo label - the toolbar's repository filter
    // also renders "rimaia", as an <option>.
    expect(screen.getAllByText("rimaia", { selector: ".task-card-repo" })).toHaveLength(2);
  });

  it("shows what belongs in an empty column instead of a blank box", async () => {
    mockBackend({ tasks: [] });
    render(<Board />);

    expect(
      await screen.findByText(/nothing queued\. cards with a finished plan/i),
    ).toBeInTheDocument();
  });

  it("shows the run-state badge a card's state maps to", async () => {
    mockBackend({ tasks: [task({ id: "a", runState: "blocked" })] });
    render(<Board />);

    expect(await screen.findByText("Blocked")).toBeInTheDocument();
  });

  it("live-refreshes on tasks:changed, treating an empty payload as a full re-read the same as any other", async () => {
    let call = 0;
    const { listenHandlers } = mockBackend({ tasks: [] });
    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") {
        call += 1;
        return call === 1 ? [] : [task({ id: "a", title: "Arrived live" })];
      }
      if (command === "list_repositories") return [repository()];
      throw new Error(`unexpected command: ${command}`);
    });

    render(<Board />);
    await waitFor(() => expect(call).toBe(1));
    expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function));

    // The shell forwarder's empty-array "re-read this entity wholesale"
    // signal (ADR-0018) — read exactly like any other payload here, since
    // the board always re-reads the whole (filtered) list regardless.
    act(() => listenHandlers["tasks:changed"]?.({ payload: [] }));
    await waitFor(() => expect(call).toBe(2));

    act(() => listenHandlers["tasks:changed"]?.({ payload: ["a"] }));
    expect(await screen.findByText("Arrived live")).toBeInTheDocument();
    expect(call).toBe(3);
  });

  it("unsubscribes from both tasks:changed and repositories:changed on unmount", async () => {
    const { unlistenSpies } = mockBackend({ tasks: [] });

    const { unmount } = render(<Board />);
    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("tasks:changed", expect.any(Function)),
    );
    await waitFor(() =>
      expect(mockListen).toHaveBeenCalledWith("repositories:changed", expect.any(Function)),
    );

    unmount();

    expect(unlistenSpies["tasks:changed"]).toHaveBeenCalled();
    expect(unlistenSpies["repositories:changed"]).toHaveBeenCalled();
  });

  it("opens a card's detail panel on click and closes it on Escape", async () => {
    mockBackend({ tasks: [task({ id: "a", title: "Wire the board" })] });
    render(<Board />);

    fireEvent.click(await screen.findByText("Wire the board"));
    expect(
      await screen.findByRole("complementary", { name: "Task: Wire the board" }),
    ).toBeInTheDocument();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("complementary", { name: "Task: Wire the board" })).toBeNull(),
    );
  });

  it("does not close the panel on Escape while a keyboard drag is in progress", async () => {
    // Fix pass finding 9: dnd-kit's own `KeyboardSensor` also cancels a
    // drag on Escape (its listener is on `document`, ahead of this board's
    // own `window` listener in the bubble order), and nothing previously
    // stopped the same keypress from also closing the panel underneath it.
    mockBackend({
      tasks: [task({ id: "a", title: "Wire the board" }), task({ id: "b", title: "Second" })],
    });
    render(<Board />);

    fireEvent.click(await screen.findByText("Wire the board"));
    await screen.findByRole("complementary", { name: "Task: Wire the board" });

    const cardA = screen.getByText("Wire the board").closest<HTMLElement>(".task-card")!;
    cardA.focus();
    // Space is a real, coordinate-independent drag activation - it does not
    // need the layout jsdom cannot provide (see `Board.tsx`'s own comment on
    // `isBelowMidpoint`).
    fireEvent.keyDown(cardA, { key: " ", code: "Space" });
    await waitFor(() => expect(cardA).toHaveClass("task-card-dragging"));

    // Fired on the card, not `window` directly, so it bubbles through
    // `document` first - the same order a real keypress would take, and the
    // order dnd-kit's own cancel-on-Escape handler depends on.
    fireEvent.keyDown(cardA, { key: "Escape", code: "Escape" });
    await waitFor(() => expect(cardA).not.toHaveClass("task-card-dragging"));
    expect(
      screen.getByRole("complementary", { name: "Task: Wire the board" }),
    ).toBeInTheDocument();
  });

  it("announces a card's title, not its raw id, when a keyboard drag starts", async () => {
    // Fix pass finding 14: `<DndContext>` with no `accessibility` prop falls
    // back to dnd-kit's own default announcements, which read `active.id`
    // literally - a task UUID (seam-contract D10) - not the title
    // `describeDragEntity` resolves it to. The drag *result* is not provable
    // in jsdom (no layout engine), but the announcement is real DOM state:
    // dnd-kit renders it into a `role="status"` live region
    // (`@dnd-kit/accessibility`'s `LiveRegion`), and Space is a real,
    // coordinate-independent activation that reaches it without needing any
    // rect.
    mockBackend({ tasks: [task({ id: "a", title: "Wire the board" })] });
    render(<Board />);

    const cardA = (await screen.findByText("Wire the board")).closest<HTMLElement>(".task-card")!;
    cardA.focus();
    fireEvent.keyDown(cardA, { key: " ", code: "Space" });

    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent('Picked up "Wire the board".'),
    );
  });

  it("saves an unblurred plan edit when the panel is closed on Escape", async () => {
    // Item 3's silent data loss, proved through the real closing path rather
    // than `PlanEditor`'s own isolated unmount test: `Esc` is `Board`'s own
    // handler (`selectedTaskId` -> `null`), which unmounts `TaskDetailPanel`
    // - and with it `PlanEditor` - exactly like selecting a different card
    // does. A regression here would mean the wiring between the two
    // components lost the edit even though `PlanEditor` in isolation is
    // proven not to.
    mockBackend({ tasks: [task({ id: "a", title: "Wire the board", plan: "old plan" })] });
    render(<Board />);

    fireEvent.click(await screen.findByText("Wire the board"));
    const plan = await screen.findByLabelText("Plan");
    expect(plan).toHaveValue("old plan");

    fireEvent.change(plan, { target: { value: "typed then escaped" } });
    fireEvent.keyDown(window, { key: "Escape" });

    await waitFor(() =>
      expect(screen.queryByRole("complementary", { name: "Task: Wire the board" })).toBeNull(),
    );
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "a",
      patch: { plan: "typed then escaped" },
    });
  });

  it("focuses the search input on / and filters cards by title", async () => {
    mockBackend({
      tasks: [task({ id: "a", title: "Wire the board" }), task({ id: "b", title: "Write docs" })],
    });
    render(<Board />);
    await screen.findByText("Wire the board");

    fireEvent.keyDown(window, { key: "/" });
    const search = screen.getByRole("searchbox", { name: "Search task titles" });
    expect(search).toHaveFocus();

    fireEvent.change(search, { target: { value: "board" } });
    expect(screen.getByText("Wire the board")).toBeInTheDocument();
    expect(screen.queryByText("Write docs")).toBeNull();
  });

  it("does not treat n as a shortcut while the search input has focus", async () => {
    mockBackend({ tasks: [] });
    render(<Board />);
    const search = await screen.findByRole("searchbox", { name: "Search task titles" });

    search.focus();
    fireEvent.keyDown(search, { key: "n" });

    expect(mockInvoke).not.toHaveBeenCalledWith("create_task", expect.anything());
  });

  it("creates a task with n and opens it, defaulting to the first repository when none is filtered", async () => {
    const repo = repository({ id: "repo-9", name: "other-repo" });
    const created = task({ id: "new-1", repositoryId: "repo-9", title: "Untitled task" });
    let listedTasks: Task[] = [];
    mockListen.mockResolvedValue(vi.fn());
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "list_tasks") return listedTasks;
      if (command === "list_repositories") return [repo];
      if (command === "create_task") {
        expect(args).toEqual({ input: { repositoryId: "repo-9", title: "Untitled task" } });
        listedTasks = [created];
        return created;
      }
      throw new Error(`unexpected command: ${command} ${JSON.stringify(args)}`);
    });

    render(<Board />);
    // `handleNewTask` itself checks `repositories.length` (not only the
    // button's `disabled`), so "n" pressed before `list_repositories`
    // resolves would be a legitimate no-op - wait for the repository filter
    // to show the fetched repository first, the same thing a real user's
    // "n" keypress is implicitly ordered after by having seen the board.
    await screen.findByRole("option", { name: "other-repo" });

    fireEvent.keyDown(window, { key: "n" });

    expect(
      await screen.findByRole("complementary", { name: "Task: Untitled task" }),
    ).toBeInTheDocument();
  });

  it("moves focus between cards with arrow keys, including through the search filter", async () => {
    mockBackend({
      tasks: [
        task({ id: "a", title: "alpha wire", position: 0 }),
        task({ id: "z", title: "zzz hidden", position: 1 }),
        task({ id: "c", title: "gamma wire", position: 2 }),
      ],
    });
    render(<Board />);

    const cardA = (await screen.findByText("alpha wire")).closest<HTMLElement>(".task-card")!;
    cardA.focus();
    fireEvent.keyDown(cardA, { key: "ArrowDown", code: "ArrowDown" });
    expect(document.activeElement).toHaveAttribute("data-task-id", "z");

    // Filtered: "zzz hidden" is hidden by the search, so arrow-down from
    // "alpha wire" must skip straight to the next *visible* card ("c"), not
    // the hidden one it would otherwise land on (fix pass finding 4/6 - this
    // is the DOM-level assertion nothing in the suite made before).
    const search = screen.getByRole("searchbox", { name: "Search task titles" });
    fireEvent.change(search, { target: { value: "wire" } });
    cardA.focus();
    fireEvent.keyDown(cardA, { key: "ArrowDown", code: "ArrowDown" });
    expect(document.activeElement).toHaveAttribute("data-task-id", "c");
  });

  it("shows the backend's own rejection reason in a banner when a move is refused", async () => {
    const tasks = [task({ id: "a", column: "not_ready", plan: null, title: "Empty plan task" })];
    mockBackend({ tasks });
    render(<Board />);
    await screen.findByText("Empty plan task");

    mockInvoke.mockImplementation(async (command) => {
      if (command === "list_tasks") return tasks;
      if (command === "move_task") {
        throw { code: "invalid", message: 'cannot put "a" in ready without a plan' };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    expect(dndContextSpy.onDragEnd).toBeTypeOf("function");
    await act(async () => {
      dndContextSpy.onDragEnd?.(fakeDragEndEvent("a", "ready"));
    });

    expect(
      await screen.findByText('cannot put "a" in ready without a plan'),
    ).toBeInTheDocument();
  });
});
