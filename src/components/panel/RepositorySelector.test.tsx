import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { RepositorySelector } from "./RepositorySelector";
import type { Repository } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts` — see
// `StorageSection.test.tsx`'s comment.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

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

// Sorted by name, as `useRepositories` hands them to the board.
const REPOSITORIES = [repository({ id: "repo-2", name: "abby" }), repository()];

/** Every prop at its "a title and a plan, nothing else yet" value — the one
 *  state seam-contract D13 allows a task to be re-filed in. */
function props(overrides: Partial<Parameters<typeof RepositorySelector>[0]> = {}) {
  return {
    taskId: "task-1",
    repositoryId: "repo-1",
    repositories: REPOSITORIES,
    repositoryName: "rimaia",
    worktreePath: null,
    hasRuns: false,
    detailLoading: false,
    ...overrides,
  };
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockResolvedValue(undefined);
});

describe("RepositorySelector", () => {
  it("offers every registered repository, with the task's own selected", async () => {
    render(<RepositorySelector {...props()} />);

    const select = screen.getByLabelText("Task repository");
    expect(
      Array.from(select.querySelectorAll("option")).map((option) => option.textContent),
    ).toEqual(["abby", "rimaia"]);
    expect(select).toHaveValue("repo-1");
    expect(select).toBeEnabled();
  });

  it("still names where the task is filed when the board has no repository list yet", () => {
    // `list_repositories` in flight, or failed — the board renders that error
    // once, at the top; the selector's job is to keep saying where the task
    // is rather than go blank.
    render(<RepositorySelector {...props({ repositories: [] })} />);

    const select = screen.getByLabelText("Task repository");
    expect(select).toHaveValue("repo-1");
    expect(screen.getByRole("option", { name: "rimaia" })).toBeInTheDocument();
  });

  it("re-files the task under the chosen repository (D13)", async () => {
    render(<RepositorySelector {...props()} />);
    const select = screen.getByLabelText("Task repository");

    fireEvent.change(select, { target: { value: "repo-2" } });

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { repositoryId: "repo-2" },
    });
    await waitFor(() => expect(select).toBeEnabled());
    expect(select).toHaveValue("repo-2");
  });

  it("reverts and shows the service's own refusal verbatim when the move is rejected", async () => {
    // The disabled states below are only a courtesy — the rule is the
    // service's, and it refuses whatever the UI believed (ADR-0006): a run
    // this panel's `get_task` predates is exactly how the two disagree. This
    // is the message `ensure_repository_is_reassignable` actually sends,
    // count and all.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "update_task") {
        throw {
          code: "invalid",
          message:
            'cannot move "Wire the board" to another repository: 2 runs have already been recorded against it',
        };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<RepositorySelector {...props()} />);
    const select = screen.getByLabelText("Task repository");
    fireEvent.change(select, { target: { value: "repo-2" } });

    expect(
      await screen.findByText(
        'cannot move "Wire the board" to another repository: 2 runs have already been recorded against it',
      ),
    ).toBeInTheDocument();
    expect(select).toHaveValue("repo-1");
  });

  it("is disabled and names the worktree once one exists", () => {
    render(
      <RepositorySelector
        {...props({ worktreePath: "/data/worktrees/rimaia/wire-the-board" })}
      />,
    );

    expect(screen.getByLabelText("Task repository")).toBeDisabled();
    expect(
      screen.getByText(
        "Cannot move to another repository: it already has a worktree at /data/worktrees/rimaia/wire-the-board.",
      ),
    ).toBeInTheDocument();
  });

  it("is disabled and names the run once one has been recorded", () => {
    render(<RepositorySelector {...props({ hasRuns: true })} />);

    expect(screen.getByLabelText("Task repository")).toBeDisabled();
    expect(
      screen.getByText(
        "Cannot move to another repository: a run has already been recorded against it.",
      ),
    ).toBeInTheDocument();
  });

  it("names the worktree, not the run count, when both hold — the order the service refuses in", () => {
    render(
      <RepositorySelector {...props({ worktreePath: "/data/worktrees/x", hasRuns: true })} />,
    );

    expect(
      screen.getByText(
        "Cannot move to another repository: it already has a worktree at /data/worktrees/x.",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/a run has already been recorded/)).toBeNull();
  });

  it("is disabled, with nothing to explain yet, while the task's detail is still loading", () => {
    render(<RepositorySelector {...props({ detailLoading: true })} />);

    expect(screen.getByLabelText("Task repository")).toBeDisabled();
    expect(screen.queryByText(/Cannot move to another repository/)).toBeNull();
  });
});
