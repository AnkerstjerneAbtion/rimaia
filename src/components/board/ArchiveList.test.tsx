import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ArchiveList } from "./ArchiveList";
import type { TaskSummary } from "../../types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const NOW = new Date("2026-09-15T12:00:00Z");
const REPOSITORIES = new Map([["repo-1", "rimaia"]]);

beforeEach(() => {
  mockInvoke.mockReset();
});

function card(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    repositoryId: "repo-1",
    title: "Ship the thing",
    plan: "a plan",
    extraInstructions: null,
    column: "done",
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
    archivedAt: "2026-09-15T10:00:00Z",
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    blockingTitle: null,
    lastRun: null,
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "global",
    ...overrides,
  };
}

describe("ArchiveList", () => {
  it("explains what archiving keeps when there is nothing archived", () => {
    render(
      <ArchiveList
        cards={[]}
        repositoriesById={REPOSITORIES}
        now={NOW}
        onSelect={vi.fn()}
        onUnarchived={vi.fn()}
      />,
    );

    expect(screen.getByText(/keeps everything/)).toBeInTheDocument();
  });

  it("shows the repository and the column a card will come back to", () => {
    // An archived card still *has* a column — unarchiving returns it there —
    // so the row says which rather than leaving it a surprise.
    render(
      <ArchiveList
        cards={[card()]}
        repositoriesById={REPOSITORIES}
        now={NOW}
        onSelect={vi.fn()}
        onUnarchived={vi.fn()}
      />,
    );

    expect(screen.getByText(/rimaia/)).toBeInTheDocument();
    expect(screen.getByText(/Done/)).toBeInTheDocument();
  });

  it("opens the same detail panel the board opens", () => {
    const onSelect = vi.fn();
    render(
      <ArchiveList
        cards={[card()]}
        repositoriesById={REPOSITORIES}
        now={NOW}
        onSelect={onSelect}
        onUnarchived={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Ship the thing" }));

    expect(onSelect).toHaveBeenCalledWith("task-1");
  });

  it("unarchives a row and tells the caller to re-read", async () => {
    mockInvoke.mockResolvedValue({});
    const onUnarchived = vi.fn();
    render(
      <ArchiveList
        cards={[card()]}
        repositoriesById={REPOSITORIES}
        now={NOW}
        onSelect={vi.fn()}
        onUnarchived={onUnarchived}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Unarchive" }));

    expect(mockInvoke).toHaveBeenCalledWith("unarchive_task", { id: "task-1" });
    await vi.waitFor(() => expect(onUnarchived).toHaveBeenCalled());
  });

  it("surfaces a rejection instead of silently leaving the row", async () => {
    mockInvoke.mockRejectedValue({ code: "not_found", message: "no task with id task-1" });
    render(
      <ArchiveList
        cards={[card()]}
        repositoriesById={REPOSITORIES}
        now={NOW}
        onSelect={vi.fn()}
        onUnarchived={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Unarchive" }));

    expect(await screen.findByText("no task with id task-1")).toBeInTheDocument();
  });
});
