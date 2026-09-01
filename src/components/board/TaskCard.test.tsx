import type { ReactNode } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import {
  DndContext,
  KeyboardCode,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
} from "@dnd-kit/core";
import { SortableContext, sortableKeyboardCoordinates } from "@dnd-kit/sortable";

import { TaskCard } from "./TaskCard";
import type { QueueEntry, Repository, Task } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts`/`lib/events.ts` — see
// `StorageSection.test.tsx`'s own comment for why. `TaskCard`'s "Run now"
// (task 008) is what makes this file need the mock at all: nothing here
// called `invoke`/`listen` before it.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

/** `Board`'s own sensor config (narrowed keyboard codes — see its own
 *  comment on why Enter is freed up), because that is what `TaskCard` is
 *  always actually rendered under. A bare `<DndContext>` here would leave
 *  dnd-kit's *default* codes in effect, which still claim Enter, and a test
 *  against that would prove nothing about the wiring this file is testing. */
function DndHarness({ children }: { children: ReactNode }) {
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, {
      coordinateGetter: sortableKeyboardCoordinates,
      keyboardCodes: {
        start: [KeyboardCode.Space],
        cancel: [KeyboardCode.Esc],
        end: [KeyboardCode.Space],
      },
    }),
  );
  return <DndContext sensors={sensors}>{children}</DndContext>;
}

const NOW = new Date("2026-08-20T12:00:00Z");

function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "task-1",
    repositoryId: "repo-1",
    title: "Wire up the board",
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
    updatedAt: "2026-08-20T11:55:00Z",
    source: "ui",
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
    allowUnattendedRuns: true,
    createdAt: "2026-08-20T09:00:00Z",
    ...overrides,
  };
}

/** Every test's default backend: one opted-in repository, an empty queue
 *  plan (task 009's `useQueueLookup` is called unconditionally, same as
 *  `useRepositoryLookup` — see both hooks' own comments), and every `listen`
 *  subscription resolves and never fires on its own — the same shape
 *  `Board.test.tsx`'s own `mockBackend` uses, scoped to what `TaskCard`
 *  itself calls. */
function mockBackend({
  repositories = [repository()],
  queuePlan = [] as QueueEntry[],
}: { repositories?: Repository[]; queuePlan?: QueueEntry[] } = {}) {
  mockListen.mockResolvedValue(vi.fn());
  mockInvoke.mockImplementation(async (command) => {
    if (command === "list_repositories") return repositories;
    if (command === "get_queue_status") {
      return { state: "paused", runningTaskId: null, plan: queuePlan };
    }
    throw new Error(`unexpected command: ${command}`);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
});

/**
 * `useSortable` needs a `SortableContext` ancestor to compute a real index —
 * the one piece of dnd-kit machinery every test below sits inside.
 *
 * Deliberately synchronous, and awaits nothing about "Run now"'s own
 * repository lookup: most tests below are not about it, and the outer
 * `<article>` dnd-kit gives `role="button"` too, whose accessible name is the
 * concatenation of everything inside it — including the nested "Run now"
 * button's own text — so any query in *this* function broad enough to serve
 * every caller would match both elements at once. The "Run now" tests below
 * query their own button precisely (an **exact** string, never a substring
 * regex) and await its own settled state themselves.
 */
function renderCard(overrides: Partial<Parameters<typeof TaskCard>[0]> = {}) {
  const props = {
    task: task(),
    repositoryName: "rimaia",
    now: NOW,
    selected: false,
    onSelect: vi.fn(),
    registerCardRef: vi.fn(),
    onArrowNavigate: vi.fn(),
    ...overrides,
  };

  render(
    <DndHarness>
      <SortableContext items={[props.task.id]}>
        <TaskCard {...props} />
      </SortableContext>
    </DndHarness>,
  );

  return props;
}

/** Clicks "Run now" once it is actually *enabled*.
 *
 *  The button is `disabled={runNow.kind !== "ready" || starting}`, and
 *  `runNowState` cannot return `ready` until the async repository lookup has
 *  resolved — so `findByRole` happily hands back a **disabled** button and
 *  React drops the click on the floor. Locally the lookup always wins that
 *  race; on a slower CI runner it does not, and the failure reads as
 *  "expected spy to be called", which points at the handler rather than at
 *  the wait. */
async function clickRunNow() {
  const button = await screen.findByRole("button", { name: "Run now" });
  await waitFor(() => expect(button).toBeEnabled());
  fireEvent.click(button);
  return button;
}

describe("TaskCard", () => {
  beforeEach(() => {
    mockBackend();
  });

  it("shows the title, repository and relative time of last activity", async () => {
    renderCard();
    await screen.findByRole("button", { name: "Run now" });

    expect(screen.getByText("Wire up the board")).toBeInTheDocument();
    expect(screen.getByText("rimaia")).toBeInTheDocument();
    expect(screen.getByText("5m ago")).toBeInTheDocument();
  });

  it("shows no badge for an idle task", async () => {
    renderCard({ task: task({ runState: "idle" }) });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.queryByText(/running|queued|blocked|retry|failed|cancelled/i)).toBeNull();
  });

  it("shows the run-state badge for a non-idle task", async () => {
    renderCard({ task: task({ runState: "blocked" }) });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("Blocked")).toBeInTheDocument();
  });

  it("calls onSelect with the task id when clicked", async () => {
    const { onSelect } = renderCard();
    await screen.findByRole("button", { name: "Run now" });

    fireEvent.click(screen.getByText("Wire up the board"));

    expect(onSelect).toHaveBeenCalledWith("task-1");
  });

  it("registers and unregisters its DOM node for arrow-key focus navigation", async () => {
    const registerCardRef = vi.fn();
    const { unmount } = render(
      <DndContext>
        <SortableContext items={["task-1"]}>
          <TaskCard
            task={task()}
            repositoryName="rimaia"
            now={NOW}
            selected={false}
            onSelect={vi.fn()}
            registerCardRef={registerCardRef}
            onArrowNavigate={vi.fn()}
          />
        </SortableContext>
      </DndContext>,
    );
    await screen.findByRole("button", { name: "Run now" });
    expect(registerCardRef).toHaveBeenCalledWith("task-1", expect.any(HTMLElement));

    unmount();

    expect(registerCardRef).toHaveBeenLastCalledWith("task-1", null);
  });

  it("calls onArrowNavigate for an arrow key press when no drag is active", async () => {
    const { onArrowNavigate } = renderCard();
    await screen.findByRole("button", { name: "Run now" });

    fireEvent.keyDown(screen.getByText("Wire up the board").closest("article")!, {
      key: "ArrowDown",
      code: "ArrowDown",
    });

    expect(onArrowNavigate).toHaveBeenCalledWith("task-1", "ArrowDown");
  });

  it("marks the selected card so it can be styled distinctly", async () => {
    renderCard({ selected: true });
    await screen.findByRole("button", { name: "Run now" });

    expect(screen.getByText("Wire up the board").closest("article")).toHaveClass(
      "task-card-selected",
    );
  });

  it("exposes selection with aria-current, not aria-selected (invalid on a role=button)", async () => {
    renderCard({ selected: true });
    await screen.findByRole("button", { name: "Run now" });
    const article = screen.getByText("Wire up the board").closest("article")!;

    // dnd-kit's own `attributes` set `role="button"` - `aria-selected` is not
    // defined for that role, so a screen reader would expose nothing for it.
    expect(article).toHaveAttribute("role", "button");
    expect(article).toHaveAttribute("aria-current", "true");
    expect(article).not.toHaveAttribute("aria-selected");
  });

  it("does not set aria-current when not selected", async () => {
    renderCard({ selected: false });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("Wire up the board").closest("article")).not.toHaveAttribute(
      "aria-current",
    );
  });

  it("opens the task on Enter — the activation key a role=button announces itself for", async () => {
    const { onSelect } = renderCard();
    await screen.findByRole("button", { name: "Run now" });

    fireEvent.keyDown(screen.getByText("Wire up the board").closest("article")!, {
      key: "Enter",
      code: "Enter",
    });

    expect(onSelect).toHaveBeenCalledWith("task-1");
  });

  // D12 (seam-contract): `list_tasks` returns the summary projection, and
  // task 005's Scope names link count and a dependency indicator as two of
  // the six things a card must show. Zero of either renders nothing (see
  // `CardFace`'s own comment on why), so these fixtures are non-zero.
  it("shows the link count when the task has links", async () => {
    renderCard({ task: { ...task(), linkCount: 3 } });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("3 links")).toBeInTheDocument();
  });

  it("singularizes the link count for exactly one link", async () => {
    renderCard({ task: { ...task(), linkCount: 1 } });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("1 link")).toBeInTheDocument();
  });

  it("shows the dependency indicator when the task depends on other tasks", async () => {
    renderCard({ task: { ...task(), dependencyCount: 2 } });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("2 deps")).toBeInTheDocument();
  });

  it("shows neither indicator when the task has no links and no dependencies", async () => {
    renderCard();
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.queryByText(/link/i)).toBeNull();
    expect(screen.queryByText(/dep/i)).toBeNull();
  });

  // D9 (seam-contract): the card is the one place "interrupted" is ever
  // supposed to appear — off the last run's exit class, never off `runState`
  // alone (a bare `failed` run would otherwise read "Failed").
  it("reads interrupted off the last run's exit class (D9)", async () => {
    renderCard({
      task: {
        ...task({ runState: "failed" }),
        lastRun: { status: "interrupted", exitClass: "interrupted", endedAt: "2026-08-20T11:50:00Z" },
      },
    });
    await screen.findByRole("button", { name: "Run now" });
    expect(screen.getByText("Interrupted")).toBeInTheDocument();
    expect(screen.queryByText("Failed")).toBeNull();
  });

  // ---------------------------------------------------------------------------
  // "Run now" (task 008; ADR-0012).
  // ---------------------------------------------------------------------------

  describe("Run now", () => {
    // Every query below asks for the **exact** string "Run now" — a
    // substring/regex match would also catch the outer `<article>`, which
    // dnd-kit gives `role="button"` too and whose accessible name is the
    // concatenation of everything inside it, "Run now" included.

    it("is enabled once the task's repository has opted in to unattended runs", async () => {
      mockBackend({ repositories: [repository({ allowUnattendedRuns: true })] });
      renderCard();

      expect(await screen.findByRole("button", { name: "Run now" })).toBeEnabled();
    });

    it("is disabled with the reason why when the repository has not opted in", async () => {
      mockBackend({ repositories: [repository({ allowUnattendedRuns: false, name: "rimaia" })] });
      renderCard();

      expect(
        await screen.findByText(/"rimaia" has not enabled unattended agent runs/),
      ).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Run now" })).toBeDisabled();
    });

    it("is disabled while the task is already running, without claiming it is the opt-in that blocks it", async () => {
      mockBackend({ repositories: [repository({ allowUnattendedRuns: true })] });
      renderCard({ task: task({ runState: "running" }) });

      // Settles immediately — `runState` is checked before the repository
      // lookup — but still awaited so the lookup's own (irrelevant) fetch
      // resolving afterwards cannot retroactively turn this green for the
      // wrong reason.
      expect(await screen.findByRole("button", { name: "Running…" })).toBeDisabled();
      expect(screen.queryByText(/has not enabled unattended agent runs/)).toBeNull();
    });

    it("calls start_task_run with the task id when clicked", async () => {
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_repositories") return [repository({ allowUnattendedRuns: true })];
        if (command === "get_queue_status") return { state: "paused", runningTaskId: null, plan: [] };
        if (command === "start_task_run") return undefined;
        throw new Error(`unexpected command: ${command}`);
      });
      renderCard();

      await clickRunNow();

      expect(mockInvoke).toHaveBeenCalledWith("start_task_run", { taskId: "task-1" });
      // Lets the click's own `setStarting(false)` land inside `act()` rather
      // than after this test has already returned.
      await screen.findByRole("button", { name: "Run now" });
    });

    it("does not open the task panel when Run now is clicked", async () => {
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_repositories") return [repository({ allowUnattendedRuns: true })];
        if (command === "get_queue_status") return { state: "paused", runningTaskId: null, plan: [] };
        if (command === "start_task_run") return undefined;
        throw new Error(`unexpected command: ${command}`);
      });
      const { onSelect } = renderCard();

      await clickRunNow();

      expect(onSelect).not.toHaveBeenCalled();
      await waitFor(() => expect(screen.getByRole("button", { name: "Run now" })).toBeEnabled());
    });

    it("shows the backend's own rejection message when start_task_run fails", async () => {
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_repositories") return [repository({ allowUnattendedRuns: true })];
        if (command === "get_queue_status") return { state: "paused", runningTaskId: null, plan: [] };
        if (command === "start_task_run") {
          throw { code: "invalid", message: "a run is already in progress for this task" };
        }
        throw new Error(`unexpected command: ${command}`);
      });
      renderCard();

      await clickRunNow();

      expect(
        await screen.findByText("a run is already in progress for this task"),
      ).toBeInTheDocument();
    });

    it("shares one list_repositories call across every mounted card", async () => {
      mockBackend({ repositories: [repository({ allowUnattendedRuns: true })] });
      render(
        <DndHarness>
          <SortableContext items={["task-1", "task-2"]}>
            <TaskCard
              task={task()}
              repositoryName="rimaia"
              now={NOW}
              selected={false}
              onSelect={vi.fn()}
              registerCardRef={vi.fn()}
              onArrowNavigate={vi.fn()}
            />
            <TaskCard
              task={task({ id: "task-2" })}
              repositoryName="rimaia"
              now={NOW}
              selected={false}
              onSelect={vi.fn()}
              registerCardRef={vi.fn()}
              onArrowNavigate={vi.fn()}
            />
          </SortableContext>
        </DndHarness>,
      );

      await waitFor(() =>
        expect(screen.getAllByRole("button", { name: "Run now" })).toHaveLength(2),
      );
      expect(mockInvoke.mock.calls.filter(([command]) => command === "list_repositories")).toHaveLength(1);
    });
  });

  // ---------------------------------------------------------------------------
  // Queued position (task 009; ADR-0010, ADR-0012).
  // ---------------------------------------------------------------------------

  describe("Queued position", () => {
    it("shows the queue position for a ready, idle task the queue would start", async () => {
      mockBackend({
        queuePlan: [
          {
            taskId: "task-1",
            title: "Wire up the board",
            repositoryId: "repo-1",
            queuePosition: 2,
            skip: null,
          },
        ],
      });
      renderCard({ task: task({ column: "ready", runState: "idle" }) });

      expect(await screen.findByText("Queued #2")).toBeInTheDocument();
    });

    it("surfaces the reason on the card when the repository has not opted in (ADR-0012)", async () => {
      mockBackend({
        queuePlan: [
          {
            taskId: "task-1",
            title: "Wire up the board",
            repositoryId: "repo-1",
            queuePosition: null,
            skip: "unattended_runs_not_allowed",
          },
        ],
      });
      renderCard({ task: task({ column: "ready", runState: "idle" }) });

      expect(
        await screen.findByText(
          "Not queued — this repository has not enabled unattended agent runs",
        ),
      ).toBeInTheDocument();
    });

    it("shows nothing when the task is not in the queue's plan", async () => {
      mockBackend({ queuePlan: [] });
      renderCard({ task: task({ column: "ready", runState: "idle" }) });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.queryByText(/^Queued #/)).toBeNull();
      expect(screen.queryByText(/^Not queued/)).toBeNull();
    });

    it("shows nothing for a task outside the ready column, even if the plan names it", async () => {
      mockBackend({
        queuePlan: [
          {
            taskId: "task-1",
            title: "Wire up the board",
            repositoryId: "repo-1",
            queuePosition: 1,
            skip: null,
          },
        ],
      });
      renderCard({ task: task({ column: "not_ready", runState: "idle" }) });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.queryByText(/^Queued #/)).toBeNull();
    });

    it("shows nothing for a non-idle task — its own run-state badge already says why", async () => {
      mockBackend({
        queuePlan: [
          {
            taskId: "task-1",
            title: "Wire up the board",
            repositoryId: "repo-1",
            queuePosition: null,
            skip: "already_in_flight",
          },
        ],
      });
      renderCard({ task: task({ column: "ready", runState: "running" }) });
      await screen.findByRole("button", { name: "Running…" });

      expect(screen.queryByText(/^Not queued/)).toBeNull();
    });

    it("shares one get_queue_status call across every mounted card", async () => {
      mockBackend({ queuePlan: [] });
      render(
        <DndHarness>
          <SortableContext items={["task-1", "task-2"]}>
            <TaskCard
              task={task()}
              repositoryName="rimaia"
              now={NOW}
              selected={false}
              onSelect={vi.fn()}
              registerCardRef={vi.fn()}
              onArrowNavigate={vi.fn()}
            />
            <TaskCard
              task={task({ id: "task-2" })}
              repositoryName="rimaia"
              now={NOW}
              selected={false}
              onSelect={vi.fn()}
              registerCardRef={vi.fn()}
              onArrowNavigate={vi.fn()}
            />
          </SortableContext>
        </DndHarness>,
      );

      await waitFor(() =>
        expect(screen.getAllByRole("button", { name: "Run now" })).toHaveLength(2),
      );
      expect(
        mockInvoke.mock.calls.filter(([command]) => command === "get_queue_status"),
      ).toHaveLength(1);
    });

    it("does not strand a card on a stale queue position when a change event lands mid fetch", async () => {
      // Task 009's own verification report, finding 7: `loadQueueStatus`'s
      // early-return guard treated an invalidation that landed while a fetch
      // was already outstanding as a no-op, so the outstanding (now stale)
      // response overwrote the cache with nothing left to correct it.
      const taskChangedListeners: Array<(event: { payload: unknown }) => void> = [];
      mockListen.mockImplementation(async (eventName, callback) => {
        if (eventName === "tasks:changed") {
          taskChangedListeners.push(callback as (event: { payload: unknown }) => void);
        }
        return vi.fn();
      });

      let resolveFirstFetch: ((value: unknown) => void) | undefined;
      let queueStatusCalls = 0;
      mockInvoke.mockImplementation(async (command) => {
        if (command === "list_repositories") return [repository({ allowUnattendedRuns: true })];
        if (command === "get_queue_status") {
          queueStatusCalls += 1;
          if (queueStatusCalls === 1) {
            return new Promise((resolve) => {
              resolveFirstFetch = resolve;
            });
          }
          return {
            state: "paused",
            runningTaskId: null,
            plan: [
              {
                taskId: "task-1",
                title: "Wire up the board",
                repositoryId: "repo-1",
                queuePosition: 3,
                skip: null,
              },
            ],
          };
        }
        throw new Error(`unexpected command: ${command}`);
      });

      renderCard({ task: task({ column: "ready", runState: "idle" }) });
      await waitFor(() => expect(taskChangedListeners.length).toBeGreaterThan(0));

      // A task moving elsewhere fires while the very first fetch is still
      // outstanding — the exact window the shared queue cache used to drop.
      taskChangedListeners[0]?.({ payload: [] });

      // The first (now stale) fetch finally settles.
      resolveFirstFetch?.({ state: "paused", runningTaskId: null, plan: [] });

      expect(await screen.findByText("Queued #3")).toBeInTheDocument();
      expect(queueStatusCalls).toBe(2);
    });
  });

  // ---------------------------------------------------------------------------
  // The effective strategy (task 020; ADR-0016, seam-contract D12's amendment).
  //
  // Every fixture below sets the three `effective*` fields directly rather
  // than `model`/`effort`: they are what `list_tasks` resolves in Rust, and a
  // card that derived a badge from `task.model` would be a second copy of the
  // precedence chain — the thing the amendment exists to prevent.
  // ---------------------------------------------------------------------------

  describe("Execution strategy", () => {
    it("shows the model and effort a run would actually spawn with", async () => {
      renderCard({
        task: {
          ...task({ model: "sonnet", effort: "high", strategyMode: "manual" }),
          effectiveModel: "sonnet",
          effectiveEffort: "high",
          effectiveOrigin: "task",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.getByText("Sonnet · high")).toBeInTheDocument();
      expect(screen.getByTitle("Model and effort from this task")).toBeInTheDocument();
    });

    it("shows an inherited value muted, and names the link of the chain it came from", async () => {
      renderCard({
        task: {
          ...task(),
          effectiveModel: "haiku",
          effectiveEffort: "low",
          effectiveOrigin: "global",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.getByText("Haiku · low")).toHaveClass("muted");
      expect(screen.getByTitle("Model and effort from the global default")).toBeInTheDocument();
    });

    it("does not mute a value the repository chose — somebody decided that one", async () => {
      renderCard({
        task: { ...task(), effectiveModel: "opus", effectiveEffort: null, effectiveOrigin: "repository" },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.getByText("Opus")).not.toHaveClass("muted");
    });

    it("renders no badge at all when nothing is configured anywhere", async () => {
      // D12: a card renders nothing rather than a badge with nothing true in
      // it. `claude_code` is precisely "no flag reaches the command line".
      renderCard({
        task: {
          ...task(),
          effectiveModel: null,
          effectiveEffort: null,
          effectiveOrigin: "claude_code",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(document.querySelector(".task-card-strategy")).toBeNull();
    });

    it("marks a planned task whose proposal is still waiting for a decision", async () => {
      renderCard({
        task: {
          ...task({
            strategyMode: "planned",
            strategySource: "planner",
            strategyPlan: JSON.stringify({ version: 1, status: "proposed", model: "sonnet" }),
          }),
          effectiveModel: "sonnet",
          effectiveEffort: "high",
          effectiveOrigin: "task",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.getByText("Proposal")).toBeInTheDocument();
    });

    it("drops the marker once the proposal has been accepted (D17.7)", async () => {
      renderCard({
        task: {
          ...task({
            strategyMode: "planned",
            strategySource: "user",
            strategyPlan: JSON.stringify({ version: 1, status: "proposed", model: "sonnet" }),
          }),
          effectiveModel: "sonnet",
          effectiveEffort: "high",
          effectiveOrigin: "task",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.queryByText("Proposal")).toBeNull();
    });

    it("shows no marker for a planner failure — the badge already says what runs", async () => {
      renderCard({
        task: {
          ...task({
            strategyMode: "planned",
            strategySource: "planner",
            strategyPlan: JSON.stringify({ version: 1, status: "failed" }),
          }),
          effectiveModel: "haiku",
          effectiveEffort: null,
          effectiveOrigin: "global",
        },
      });
      await screen.findByRole("button", { name: "Run now" });

      expect(screen.queryByText("Proposal")).toBeNull();
      expect(screen.getByText("Haiku")).toBeInTheDocument();
    });
  });
});
