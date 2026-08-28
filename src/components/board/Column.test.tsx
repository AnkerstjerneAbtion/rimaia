import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { DndContext } from "@dnd-kit/core";

import { Column, COLUMN_EMPTY_HINTS, COLUMN_TITLES } from "./Column";
import type { Task } from "../../types";

const NOW = new Date("2026-08-20T12:00:00Z");

function task(id: string): Task {
  return {
    id,
    repositoryId: "repo-1",
    title: id,
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
    source: "ui",
  };
}

function renderColumn(cards: Task[]) {
  render(
    <DndContext>
      <Column
        column="ready"
        cards={cards}
        repositoriesById={new Map([["repo-1", "rimaia"]])}
        selectedTaskId={null}
        onSelect={vi.fn()}
        registerCardRef={vi.fn()}
        onArrowNavigate={vi.fn()}
        dragDisabled={false}
        now={NOW}
      />
    </DndContext>,
  );
}

describe("Column", () => {
  it("shows its ADR-0007 display name and card count", () => {
    renderColumn([task("a"), task("b")]);

    expect(screen.getByText(COLUMN_TITLES.ready)).toBeInTheDocument();
    expect(screen.getByText("2")).toBeInTheDocument();
  });

  it("says what belongs there instead of showing a blank box when empty", () => {
    renderColumn([]);

    expect(screen.getByText(COLUMN_EMPTY_HINTS.ready)).toBeInTheDocument();
    expect(screen.getByText("0")).toBeInTheDocument();
  });

  it("renders one card per task, resolving the repository id to its name", () => {
    renderColumn([task("a"), task("b")]);

    expect(screen.getAllByText("rimaia")).toHaveLength(2);
  });
});
