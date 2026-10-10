import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { AnalyticsView } from "./AnalyticsView";
import type { Analytics, RunCostSummary } from "../types";

// Mocked at the Tauri seam, not `lib/commands.ts` — see
// `StorageSection.test.tsx`'s own comment for why.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function analytics(overrides: Partial<Analytics> = {}): Analytics {
  return {
    period: { from: null, to: null },
    outcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 0 },
    spendUsd: 0,
    spendByDay: [],
    runsWithoutCost: 0,
    runsWithoutModel: 0,
    tasksAttempted: 0,
    tasksCompleted: 0,
    costPerCompletedTaskUsd: null,
    medianDurationSeconds: null,
    longestRun: null,
    unattendedHours: 0,
    models: [],
    strategies: [],
    plannerSpendUsd: 0,
    implementationSpendUsd: 0,
    reviewLoopSpendUsd: 0,
    reviewLoopOutcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 0 },
    subscriptionMonthlyUsd: null,
    ...overrides,
  };
}

function mockBackend(costSummary: RunCostSummary | Error, report: Analytics = analytics()) {
  mockInvoke.mockImplementation(async (command: string) => {
    if (command === "get_analytics") return report;
    if (command === "get_subscription_cost") return null;
    if (command === "get_run_cost_summary") {
      if (costSummary instanceof Error) throw costSummary;
      return costSummary;
    }
    throw new Error(`unexpected command ${command}`);
  });
}

describe("AnalyticsView", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("names the active provider as the subscription being compared against", async () => {
    // Task 032: the page must not say "Anthropic" for a provider that is not
    // Claude Code.
    mockBackend({
      medianUsd: null,
      sampleSize: 0,
      inheritCostUsd: null,
      providerDisplayName: "Ledger",
    });

    render(<AnalyticsView />);

    expect(await screen.findByText(/What you pay for Ledger each month/)).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/Anthropic|Claude/);
  });

  it("names no product when the provider cannot be read", async () => {
    mockBackend(new Error("no summary"));

    render(<AnalyticsView />);

    expect(
      await screen.findByText(/What you pay for your agent CLI each month/),
    ).toBeInTheDocument();
  });

  describe("review loops (task 037)", () => {
    const costSummary: RunCostSummary = {
      medianUsd: null,
      sampleSize: 0,
      inheritCostUsd: null,
      providerDisplayName: "Claude Code",
    };

    it("shows no review-loop group when the range holds no review or fix row", async () => {
      mockBackend(costSummary);

      render(<AnalyticsView />);

      await screen.findByText("Spent");
      expect(screen.queryByRole("heading", { name: "Review loops" })).toBeNull();
    });

    it("shows the group, with core's split of spend, when the range has review or fix rows", async () => {
      mockBackend(
        costSummary,
        analytics({
          spendUsd: 7.5,
          implementationSpendUsd: 6,
          reviewLoopSpendUsd: 1.5,
          reviewLoopOutcomes: { succeeded: 4, failed: 1, cancelled: 0, interrupted: 0, running: 0 },
        }),
      );

      render(<AnalyticsView />);

      expect(await screen.findByRole("heading", { name: "Review loops" })).toBeInTheDocument();
      expect(screen.getByText("Review and fix spend").nextSibling).toHaveTextContent("$1.50");
      expect(screen.getByText("Implementation spend").nextSibling).toHaveTextContent("$6.00");
      const group = screen.getByRole("heading", { name: "Review loops" }).closest("section");
      expect(within(group as HTMLElement).getByText("Succeeded").nextSibling).toHaveTextContent(
        "4",
      );
    });

    it("shows the group for a free review too, because the row exists", async () => {
      mockBackend(
        costSummary,
        analytics({
          reviewLoopOutcomes: { succeeded: 0, failed: 0, cancelled: 0, interrupted: 0, running: 1 },
        }),
      );

      render(<AnalyticsView />);

      expect(await screen.findByRole("heading", { name: "Review loops" })).toBeInTheDocument();
    });

    it("labels the failure rate and the median as implementation runs", async () => {
      mockBackend(
        costSummary,
        analytics({
          outcomes: { succeeded: 3, failed: 1, cancelled: 0, interrupted: 0, running: 0 },
          medianDurationSeconds: 120,
        }),
      );

      render(<AnalyticsView />);

      expect(await screen.findByText("1 failed of 4 finished implementation runs")).toBeInTheDocument();
      expect(screen.getByText("Median implementation run")).toBeInTheDocument();
    });
  });
});
