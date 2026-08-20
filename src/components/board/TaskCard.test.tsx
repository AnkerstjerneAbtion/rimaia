import type { ReactNode } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

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
import type { Task } from "../../types";

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
    ...overrides,
  };
}

/** `useSortable` needs a `SortableContext` ancestor to compute a real index -
 *  the one piece of dnd-kit machinery every test below sits inside. */
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

describe("TaskCard", () => {
  it("shows the title, repository and relative time of last activity", () => {
    renderCard();

    expect(screen.getByText("Wire up the board")).toBeInTheDocument();
    expect(screen.getByText("rimaia")).toBeInTheDocument();
    expect(screen.getByText("5m ago")).toBeInTheDocument();
  });

  it("shows no badge for an idle task", () => {
    renderCard({ task: task({ runState: "idle" }) });
    expect(screen.queryByText(/running|queued|blocked|retry|failed|cancelled/i)).toBeNull();
  });

  it("shows the run-state badge for a non-idle task", () => {
    renderCard({ task: task({ runState: "blocked" }) });
    expect(screen.getByText("Blocked")).toBeInTheDocument();
  });

  it("calls onSelect with the task id when clicked", () => {
    const { onSelect } = renderCard();

    fireEvent.click(screen.getByText("Wire up the board"));

    expect(onSelect).toHaveBeenCalledWith("task-1");
  });

  it("registers and unregisters its DOM node for arrow-key focus navigation", () => {
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

    expect(registerCardRef).toHaveBeenCalledWith("task-1", expect.any(HTMLElement));

    unmount();

    expect(registerCardRef).toHaveBeenLastCalledWith("task-1", null);
  });

  it("calls onArrowNavigate for an arrow key press when no drag is active", () => {
    const { onArrowNavigate } = renderCard();

    fireEvent.keyDown(screen.getByText("Wire up the board").closest("article")!, {
      key: "ArrowDown",
      code: "ArrowDown",
    });

    expect(onArrowNavigate).toHaveBeenCalledWith("task-1", "ArrowDown");
  });

  it("marks the selected card so it can be styled distinctly", () => {
    renderCard({ selected: true });

    expect(screen.getByText("Wire up the board").closest("article")).toHaveClass(
      "task-card-selected",
    );
  });

  it("exposes selection with aria-current, not aria-selected (invalid on a role=button)", () => {
    renderCard({ selected: true });
    const article = screen.getByText("Wire up the board").closest("article")!;

    // dnd-kit's own `attributes` set `role="button"` - `aria-selected` is not
    // defined for that role, so a screen reader would expose nothing for it.
    expect(article).toHaveAttribute("role", "button");
    expect(article).toHaveAttribute("aria-current", "true");
    expect(article).not.toHaveAttribute("aria-selected");
  });

  it("does not set aria-current when not selected", () => {
    renderCard({ selected: false });
    expect(screen.getByText("Wire up the board").closest("article")).not.toHaveAttribute(
      "aria-current",
    );
  });

  it("opens the task on Enter — the activation key a role=button announces itself for", () => {
    const { onSelect } = renderCard();

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
  it("shows the link count when the task has links", () => {
    renderCard({ task: { ...task(), linkCount: 3 } });
    expect(screen.getByText("3 links")).toBeInTheDocument();
  });

  it("singularizes the link count for exactly one link", () => {
    renderCard({ task: { ...task(), linkCount: 1 } });
    expect(screen.getByText("1 link")).toBeInTheDocument();
  });

  it("shows the dependency indicator when the task depends on other tasks", () => {
    renderCard({ task: { ...task(), dependencyCount: 2 } });
    expect(screen.getByText("2 deps")).toBeInTheDocument();
  });

  it("shows neither indicator when the task has no links and no dependencies", () => {
    renderCard();
    expect(screen.queryByText(/link/i)).toBeNull();
    expect(screen.queryByText(/dep/i)).toBeNull();
  });

  // D9 (seam-contract): the card is the one place "interrupted" is ever
  // supposed to appear — off the last run's exit class, never off `runState`
  // alone (a bare `failed` run would otherwise read "Failed").
  it("reads interrupted off the last run's exit class (D9)", () => {
    renderCard({
      task: {
        ...task({ runState: "failed" }),
        lastRun: { status: "interrupted", exitClass: "interrupted", endedAt: "2026-08-20T11:50:00Z" },
      },
    });
    expect(screen.getByText("Interrupted")).toBeInTheDocument();
    expect(screen.queryByText("Failed")).toBeNull();
  });
});
