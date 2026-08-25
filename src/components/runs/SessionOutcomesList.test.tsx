import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SessionOutcomesList } from "./SessionOutcomesList";
import type { SessionOutcome } from "./SessionOutcomesList";

function outcome(overrides: Partial<SessionOutcome> = {}): SessionOutcome {
  return {
    taskId: "task-1",
    title: "Add truncate_slug",
    repositoryName: "rimaia",
    status: "succeeded",
    exitClass: "success",
    endedAt: "2026-08-20T23:00:00Z",
    ...overrides,
  };
}

describe("SessionOutcomesList", () => {
  it("shows a placeholder before anything has finished", () => {
    render(<SessionOutcomesList outcomes={[]} />);

    expect(
      screen.getByText(/Nothing has finished yet/),
    ).toBeInTheDocument();
  });

  it("shows the title, repository and outcome for a finished run", () => {
    render(<SessionOutcomesList outcomes={[outcome()]} />);

    expect(screen.getByText("Add truncate_slug")).toBeInTheDocument();
    expect(screen.getByText("rimaia")).toBeInTheDocument();
    expect(screen.getByText("Succeeded")).toBeInTheDocument();
  });

  it("reads the word 'Interrupted' off the exit class, not off a generic failure label (D9)", () => {
    render(
      <SessionOutcomesList
        outcomes={[outcome({ status: "interrupted", exitClass: "interrupted" })]}
      />,
    );

    expect(screen.getByText("Interrupted")).toBeInTheDocument();
  });

  it("renders one entry per outcome, newest first as given", () => {
    render(
      <SessionOutcomesList
        outcomes={[
          outcome({ taskId: "task-2", title: "Second", endedAt: "2026-08-20T23:05:00Z" }),
          outcome({ taskId: "task-1", title: "First", endedAt: "2026-08-20T23:00:00Z" }),
        ]}
      />,
    );

    const items = screen.getAllByRole("listitem").map((li) => li.textContent);
    expect(items[0]).toContain("Second");
    expect(items[1]).toContain("First");
  });

  it("falls back to the run's own status when no exit class is recorded", () => {
    render(<SessionOutcomesList outcomes={[outcome({ exitClass: null, status: "cancelled" })]} />);

    expect(screen.getByText("cancelled")).toBeInTheDocument();
  });
});
