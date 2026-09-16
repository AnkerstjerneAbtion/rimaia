import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { ArchiveTaskSection } from "./ArchiveTaskSection";
import type { Repository, Task } from "../../types";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

beforeEach(() => {
  mockInvoke.mockReset();
});

function task(overrides: Partial<Task> = {}): Task {
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
    archivedAt: null,
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
    maxConcurrency: 1,
    createdAt: "2026-08-20T09:00:00Z",
    onArchive: "none",
    onArchiveScript: null,
    ...overrides,
  };
}

const CLEAN = { taskId: "task-1", title: "Ship the thing", archivedAt: "x", cleanup: { kind: "nothing" } };

describe("ArchiveTaskSection", () => {
  it("does not archive on the first click — it asks first", () => {
    render(
      <ArchiveTaskSection task={task()} repository={repository()} onArchived={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(mockInvoke).not.toHaveBeenCalled();
    expect(
      screen.getByRole("alertdialog", { name: 'Confirm archive "Ship the thing"' }),
    ).toBeInTheDocument();
  });

  it("says nothing about cleanup when the repository cleans nothing up", () => {
    render(
      <ArchiveTaskSection task={task()} repository={repository()} onArchived={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(screen.queryByText(/worktree will be deleted/)).not.toBeInTheDocument();
    expect(screen.queryByText(/cleanup script will run/)).not.toBeInTheDocument();
  });

  it("names the worktree deletion in the confirmation, before the click that does it", () => {
    // "Archive" and "Archive, and delete a 900 MB checkout" must not be the
    // same sentence (ADR-0025).
    render(
      <ArchiveTaskSection
        task={task()}
        repository={repository({ onArchive: "remove_worktree" })}
        onArchived={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(screen.getByText(/worktree will be deleted/)).toBeInTheDocument();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("names the script, and says Rimaia guards nothing about it", () => {
    render(
      <ArchiveTaskSection
        task={task()}
        repository={repository({ onArchive: "script", onArchiveScript: "/opt/teardown.sh" })}
        onArchived={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(screen.getByText(/\/opt\/teardown\.sh/)).toBeInTheDocument();
    expect(screen.getByText(/none of its own guards/)).toBeInTheDocument();
  });

  it("archives and closes the panel once the confirmation is accepted", async () => {
    mockInvoke.mockResolvedValue(CLEAN);
    const onArchived = vi.fn();
    render(
      <ArchiveTaskSection task={task()} repository={repository()} onArchived={onArchived} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(mockInvoke).toHaveBeenCalledWith("archive_task", { id: "task-1" });
    await vi.waitFor(() => expect(onArchived).toHaveBeenCalled());
  });

  it("stays open and reports a cleanup that did not go cleanly", async () => {
    // The archive committed; the cleanup did not. Closing the panel silently
    // would make the user go looking for news they explicitly asked for.
    mockInvoke.mockResolvedValue({
      ...CLEAN,
      cleanup: { kind: "failed", reason: "3 uncommitted changes" },
    });
    const onArchived = vi.fn();
    render(
      <ArchiveTaskSection task={task()} repository={repository()} onArchived={onArchived} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(await screen.findByText("3 uncommitted changes")).toBeInTheDocument();
    expect(onArchived).not.toHaveBeenCalled();
  });

  it("shows the refusal when archiving is rejected", async () => {
    mockInvoke.mockRejectedValue({ code: "invalid", message: "Cancel the run first" });
    render(
      <ArchiveTaskSection task={task()} repository={repository()} onArchived={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));
    fireEvent.click(screen.getByRole("button", { name: "Archive task" }));

    expect(await screen.findByText("Cancel the run first")).toBeInTheDocument();
  });

  it("offers the way back for a task that is already archived", async () => {
    mockInvoke.mockResolvedValue(task({ archivedAt: null }));
    const onArchived = vi.fn();
    render(
      <ArchiveTaskSection
        task={task({ archivedAt: "2026-09-15T10:00:00Z" })}
        repository={repository()}
        onArchived={onArchived}
      />,
    );

    expect(screen.queryByRole("button", { name: "Archive task" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Unarchive" }));

    expect(mockInvoke).toHaveBeenCalledWith("unarchive_task", { id: "task-1" });
    await vi.waitFor(() => expect(onArchived).toHaveBeenCalled());
  });
});
