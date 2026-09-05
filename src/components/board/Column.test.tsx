import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { DndContext } from "@dnd-kit/core";

import { Column, COLUMN_EMPTY_HINTS, COLUMN_TITLES, columnStats } from "./Column";
import type { ExitClass, Task } from "../../types";

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
        pickedTaskIds={new Set<string>()}
        onPick={vi.fn()}
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

  it("says what the run queue reads, on the one column it reads from (ADR-0007)", () => {
    renderColumn([task("a")]);

    expect(screen.getByText("The run queue, top to bottom")).toBeInTheDocument();
  });
});

// The column header's aggregate. Pure, so it is driven directly rather than
// through a render — the point of the function is which kinds it counts and
// which it drops, and a DOM assertion would only re-check the loop that maps
// them.
describe("columnStats", () => {
  type Card = Parameters<typeof columnStats>[0][number];

  function card(overrides: Partial<Card> = {}): Card {
    return { ...task("a"), ...overrides };
  }

  function lastRun(exitClass: ExitClass): NonNullable<Card["lastRun"]> {
    return { status: "failed", exitClass, endedAt: "2026-08-20T11:30:00Z", resumeAfter: null };
  }

  it("says nothing about a column where nothing is happening", () => {
    expect(columnStats([card({ runState: "idle" }), card({ runState: "idle" })])).toEqual([]);
  });

  it("counts each state separately and names it", () => {
    const stats = columnStats([
      card({ runState: "running" }),
      card({ runState: "running" }),
      card({ runState: "queued" }),
    ]);

    expect(stats).toEqual([
      { tone: "running", count: 2, label: "running" },
      { tone: "queued", count: 1, label: "queued" },
    ]);
  });

  it("counts what the card actually shows, not what run_state spells", () => {
    // Both renderings `cardBadge` derives rather than reads: D9's interrupted
    // (off the last run's exit class) and ADR-0008's blocked (off the derived
    // flag, on a row whose state is `idle`). Counting `runState` directly
    // would put the first under "failed" — right word, wrong reason — and lose
    // the second entirely.
    const stats = columnStats([
      card({ runState: "failed", lastRun: lastRun("interrupted") }),
      card({ runState: "idle", blockedByIncomplete: true }),
    ]);

    expect(stats).toEqual([
      { tone: "failed", count: 1, label: "failed" },
      { tone: "blocked", count: 1, label: "blocked" },
    ]);
  });

  it("says nothing for a cancelled card — nothing is happening to it either", () => {
    expect(columnStats([card({ runState: "cancelled" })])).toEqual([]);
  });

  it("keeps the three most urgent kinds, dropping only the calmest", () => {
    // A 160px column header cannot carry five chips, so the aggregate is
    // capped — and the cap has to fall on the two states nobody needs to act
    // on, never on a failure.
    const stats = columnStats([
      card({ runState: "queued" }),
      card({ runState: "running" }),
      card({ runState: "waiting_retry" }),
      card({ runState: "idle", blockedByIncomplete: true }),
      card({ runState: "failed", lastRun: lastRun("fatal") }),
    ]);

    expect(stats.map((stat) => stat.tone)).toEqual(["failed", "waiting", "blocked"]);
  });
});
