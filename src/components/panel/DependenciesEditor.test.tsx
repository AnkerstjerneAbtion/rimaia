import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { DependenciesEditor } from "./DependenciesEditor";
import type { BoardColumn, TaskSummary } from "../../types";

// Mocked at the Tauri seam, never at `lib/commands.ts` — a test that mocks the
// wrapper proves the component calls the wrapper, and says nothing about the
// command name or the argument shape actually going over the boundary, which
// is the half that can be wrong without typechecking noticing.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function summary(id: string, title: string, column: BoardColumn): TaskSummary {
  return {
    id,
    repositoryId: "repo-1",
    title,
    plan: "a plan",
    extraInstructions: null,
    column,
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
    createdAt: "2026-08-20T12:00:00Z",
    updatedAt: "2026-08-20T12:00:00Z",
    source: "ui",
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    blockingTitle: null,
    lastRun: null,
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
  };
}

/** The board this editor's picker reads, as `list_tasks` would answer it. */
const BOARD = [
  summary("task-1", "Call it from the UI", "ready"),
  summary("task-2", "Add the API endpoint", "in_review"),
  summary("task-3", "Add the schema", "ready"),
];

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation((command: string) =>
    command === "list_tasks" ? Promise.resolve(BOARD) : Promise.resolve(undefined),
  );
});

function renderEditor(dependsOn: string[], onChanged = vi.fn()) {
  render(
    <DependenciesEditor
      taskId="task-1"
      repositoryId="repo-1"
      dependsOn={dependsOn}
      loading={false}
      onChanged={onChanged}
    />,
  );
  return onChanged;
}

describe("DependenciesEditor", () => {
  it("lists each edge with its resolved status", async () => {
    renderEditor(["task-2", "task-3"]);

    // ADR-0008's satisfaction rule, as the panel renders it: `in_review`
    // satisfies, `ready` does not.
    expect(await screen.findByText(/Satisfied — In review/)).toBeInTheDocument();
    expect(screen.getByText(/Waiting — Ready for implementation/)).toBeInTheDocument();
    expect(screen.getByText("Add the API endpoint")).toBeInTheDocument();
    expect(screen.getByText("Add the schema")).toBeInTheDocument();
  });

  it("offers only other tasks in the same repository that are not already dependencies", async () => {
    renderEditor(["task-2"]);

    await screen.findByText("Add the API endpoint");
    const options = screen
      .getAllByRole("option")
      .map((option) => (option as HTMLOptionElement).text);

    // Not itself (task-1), not the edge it already has (task-2).
    expect(options).toEqual(["Choose a task…", "Add the schema"]);
  });

  it("narrows the picker by the search box", async () => {
    renderEditor([]);
    await screen.findByLabelText("Task to depend on");

    fireEvent.change(screen.getByLabelText("Search for a task to depend on"), {
      target: { value: "schema" },
    });

    const options = screen
      .getAllByRole("option")
      .map((option) => (option as HTMLOptionElement).text);
    expect(options).toEqual(["Choose a task…", "Add the schema"]);
  });

  it("adds a dependency by sending the whole resulting set", async () => {
    // Replace, never merge: the service takes the complete set every time, and
    // cycle detection has to see it to be sound.
    const onChanged = renderEditor(["task-2"]);
    await screen.findByText("Add the API endpoint");

    fireEvent.change(screen.getByLabelText("Task to depend on"), {
      target: { value: "task-3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add dependency" }));

    expect(mockInvoke).toHaveBeenCalledWith("set_task_dependencies", {
      taskId: "task-1",
      dependsOn: ["task-2", "task-3"],
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("removes a dependency only after the inline confirm", async () => {
    const onChanged = renderEditor(["task-2", "task-3"]);
    await screen.findByText("Add the API endpoint");

    fireEvent.click(
      screen.getByRole("button", { name: "Remove the dependency on Add the API endpoint" }),
    );
    expect(mockInvoke).not.toHaveBeenCalledWith("set_task_dependencies", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

    expect(mockInvoke).toHaveBeenCalledWith("set_task_dependencies", {
      taskId: "task-1",
      dependsOn: ["task-3"],
    });
    await waitFor(() => expect(onChanged).toHaveBeenCalled());
  });

  it("renders the service's cycle refusal verbatim and does not refetch", async () => {
    // Task 011's acceptance criterion: "creating a cycle is rejected in both UI
    // and MCP, naming the path". The path is in the message (seam-contract D8),
    // so the panel's job is to show it rather than to summarise it.
    const message =
      'cannot save these dependencies: they would create a cycle — "Call it from the UI" ' +
      'depends on "Add the schema" depends on "Call it from the UI"';
    const onChanged = vi.fn();
    mockInvoke.mockImplementation((command: string) => {
      if (command === "list_tasks") return Promise.resolve(BOARD);
      return Promise.reject({ code: "invalid", message });
    });
    renderEditor([], onChanged);
    await screen.findByLabelText("Task to depend on");

    fireEvent.change(screen.getByLabelText("Task to depend on"), {
      target: { value: "task-3" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add dependency" }));

    expect(await screen.findByText(message)).toBeInTheDocument();
    expect(onChanged).not.toHaveBeenCalled();
  });

  it("says so rather than guessing when a dependency is not on this board", async () => {
    // A row the board read did not return — deleted, or moved by a hand-edit.
    // "Waiting" would be a guess; the id is at least something to search for.
    renderEditor(["task-gone"]);

    expect(await screen.findByText("Not on this board")).toBeInTheDocument();
    expect(screen.getByText("task-gone")).toBeInTheDocument();
  });
});
