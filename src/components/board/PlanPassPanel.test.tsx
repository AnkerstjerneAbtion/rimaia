import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PlanPassPanel } from "./PlanPassPanel";
import type { PlanPass, PlanProgress, PlanResult } from "../../types";

function result(overrides: Partial<PlanResult> = {}): PlanResult {
  return {
    taskId: "task-1",
    title: "Wire up the board",
    outcome: "planned",
    model: "sonnet",
    effort: "high",
    rationale: "The plan names a migration and a command surface.",
    costUsd: 0.04,
    skip: null,
    reason: null,
    ...overrides,
  };
}

function pass(overrides: Partial<PlanPass> = {}): PlanPass {
  return {
    results: [result()],
    planned: 1,
    skipped: 0,
    spentUsd: 0.04,
    cancelled: false,
    ...overrides,
  };
}

function renderPanel(props: Partial<Parameters<typeof PlanPassPanel>[0]> = {}) {
  const merged = {
    running: false,
    progress: null as PlanProgress | null,
    pass: null as PlanPass | null,
    error: null,
    onCancel: vi.fn(),
    onDismiss: vi.fn(),
    ...props,
  };
  render(<PlanPassPanel {...merged} />);
  return merged;
}

describe("PlanPassPanel", () => {
  it("renders nothing when no pass has been started", () => {
    const { container } = render(
      <PlanPassPanel
        running={false}
        progress={null}
        pass={null}
        error={null}
        onCancel={vi.fn()}
        onDismiss={vi.fn()}
      />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  // Task 023: "which card is being planned, how many are left, and the running
  // total spent, since the whole point is that the user chose to spend it".
  it("names the card being planned, the position in the pass, and what it has spent", () => {
    renderPanel({
      running: true,
      progress: {
        completed: 3,
        total: 10,
        spentUsd: 0.12,
        result: result({ title: "Third card" }),
      },
    });

    expect(screen.getByText("Planning 3 of 10")).toBeInTheDocument();
    expect(screen.getByText("Third card")).toBeInTheDocument();
    expect(screen.getByText("$0.12 spent")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
  });

  it("shows model, effort and the one-line rationale for each planned card", () => {
    renderPanel({ pass: pass() });

    expect(screen.getByText("sonnet · high")).toBeInTheDocument();
    expect(
      screen.getByText("The plan names a migration and a command surface."),
    ).toBeInTheDocument();
    expect(screen.getByText("1 planned, 0 skipped")).toBeInTheDocument();
    expect(screen.getByText("$0.04 spent")).toBeInTheDocument();
  });

  // The failure mode task 023 names: an empty column silently doing nothing.
  it("reports every card it passed over, with the reason", () => {
    renderPanel({
      pass: pass({
        results: [
          result({
            outcome: "skipped",
            skip: "already_proposed",
            reason: "already carries a proposal — clear it, or use Re-plan on the card",
            model: null,
            effort: null,
            rationale: null,
            costUsd: null,
          }),
        ],
        planned: 0,
        skipped: 1,
        spentUsd: 0,
      }),
    });

    expect(screen.getByText("0 planned, 1 skipped")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Already proposed — already carries a proposal — clear it, or use Re-plan on the card",
      ),
    ).toBeInTheDocument();
  });

  it("says so plainly when the selection held nothing that could be planned", () => {
    renderPanel({ pass: pass({ results: [], planned: 0, skipped: 0, spentUsd: 0 }) });

    expect(
      screen.getByText(/Nothing in this selection could be planned/),
    ).toBeInTheDocument();
  });

  it("says a cancelled pass was cancelled, and still shows what it got through", () => {
    renderPanel({ pass: pass({ cancelled: true }) });

    expect(screen.getByText("Planning pass cancelled")).toBeInTheDocument();
    // Proposals already written stay written, so the summary still lists them.
    expect(screen.getByText("sonnet · high")).toBeInTheDocument();
  });
});
