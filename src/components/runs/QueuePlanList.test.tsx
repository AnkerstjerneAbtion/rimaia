import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { QueuePlanList } from "./QueuePlanList";
import type { QueueEntry } from "../../types";

function entry(overrides: Partial<QueueEntry> = {}): QueueEntry {
  return {
    taskId: "task-1",
    title: "Add truncate_slug",
    repositoryId: "repo-1",
    queuePosition: 1,
    skip: null,
    resumeAfter: null,
    ...overrides,
  };
}

describe("QueuePlanList", () => {
  it("shows a placeholder when the ready column is empty", () => {
    render(<QueuePlanList plan={[]} />);

    expect(screen.getByText("Nothing in the ready column right now.")).toBeInTheDocument();
  });

  it("shows the queue position for a claimable task", () => {
    render(<QueuePlanList plan={[entry({ queuePosition: 3, skip: null })]} />);

    expect(screen.getByText("Add truncate_slug")).toBeInTheDocument();
    expect(screen.getByText("#3")).toBeInTheDocument();
  });

  it("shows the reason for a task the queue will pass over", () => {
    render(
      <QueuePlanList
        plan={[entry({ queuePosition: null, skip: "unattended_runs_not_allowed" })]}
      />,
    );

    expect(
      screen.getByText(
        "Not queued — this repository has not enabled unattended agent runs",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/^#/)).toBeNull();
  });

  it("renders every entry in the order the plan names, board order", () => {
    render(
      <QueuePlanList
        plan={[
          entry({ taskId: "task-1", title: "First", queuePosition: 1 }),
          entry({ taskId: "task-2", title: "Second", queuePosition: 2 }),
        ]}
      />,
    );

    const titles = screen.getAllByText(/^(First|Second)$/).map((el) => el.textContent);
    expect(titles).toEqual(["First", "Second"]);
  });

  it("shows a distinct reason for each skip reason", () => {
    render(
      <QueuePlanList
        plan={[
          entry({ taskId: "task-1", title: "A", queuePosition: null, skip: "dependency_not_satisfied" }),
          entry({ taskId: "task-2", title: "B", queuePosition: null, skip: "already_in_flight" }),
          entry({ taskId: "task-3", title: "C", queuePosition: null, skip: "needs_attention" }),
        ]}
      />,
    );

    expect(screen.getByText("Not queued — waiting on a dependency")).toBeInTheDocument();
    expect(screen.getByText("Not queued — already started")).toBeInTheDocument();
    expect(screen.getByText("Not queued — the last run did not succeed")).toBeInTheDocument();
  });
});
