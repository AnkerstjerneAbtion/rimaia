import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { TeamsProvider } from "../../lib/teams";
import { ACME, PERSONAL } from "../../test/web";
import type { CopyTarget, TeamListing } from "../../types";
import { CopyToTeamSection } from "./CopyToTeamSection";

// Transport-agnostic: the section only calls its two wrappers, so the invoke
// mock is enough.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn(), isTauri: vi.fn(() => false) }));

const mockInvoke = vi.mocked(invoke);

const TARGETS: CopyTarget[] = [
  { teamId: PERSONAL.id, teamName: "Personal", repositories: [{ id: "repo-notes", name: "notes" }] },
  {
    teamId: ACME.id,
    teamName: "Acme platform",
    repositories: [
      { id: "repo-web", name: "web" },
      { id: "repo-api", name: "api" },
    ],
  },
];

function renderSection(teams: TeamListing[]) {
  return render(
    <TeamsProvider value={{ teams, current: teams[0], choose: () => {}, reload: async () => {} }}>
      <CopyToTeamSection taskId="task-1" />
    </TeamsProvider>,
  );
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockInvoke.mockImplementation(async (command) => {
    if (command === "list_copy_targets") return TARGETS;
    if (command === "copy_task_to_team") return { id: "task-copy" };
    throw new Error(`unexpected ${String(command)}`);
  });
});

afterEach(() => {
  cleanup();
});

describe("copying a task to another team", () => {
  it("is absent for a person in one team, and without a switcher", () => {
    const { container } = renderSection([PERSONAL]);
    expect(container).toBeEmptyDOMElement();
    cleanup();

    const solo = render(<CopyToTeamSection taskId="task-1" />);
    expect(solo.container).toBeEmptyDOMElement();
    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("offers only the chosen team's repositories, and copies into one", async () => {
    renderSection([PERSONAL, ACME]);

    fireEvent.click(screen.getByRole("button", { name: "Copy to team…" }));
    const team = await screen.findByRole("combobox", { name: "Team" });
    expect(
      screen.getByText(
        "The copy brings the plan and nothing else: no runs, links, dependencies or assignee. It starts in Not ready.",
      ),
    ).toBeInTheDocument();
    const repository = screen.getByRole("combobox", { name: "Repository" });
    expect(repository).toBeDisabled();

    fireEvent.change(team, { target: { value: ACME.id } });
    const offered = within(repository)
      .getAllByRole("option")
      .map((option) => option.textContent);
    expect(offered).toEqual(["Choose a repository", "web", "api"]);

    fireEvent.change(repository, { target: { value: "repo-api" } });
    fireEvent.click(screen.getByRole("button", { name: "Copy task" }));

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith("copy_task_to_team", {
        taskId: "task-1",
        repositoryId: "repo-api",
      }),
    );
    expect(await screen.findByRole("status")).toHaveTextContent(
      "Copied to Acme platform, as a new task in Not ready.",
    );
  });
});
