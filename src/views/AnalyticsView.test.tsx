import { render, screen } from "@testing-library/react";
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
    subscriptionMonthlyUsd: null,
    ...overrides,
  };
}

function mockBackend(costSummary: RunCostSummary | Error) {
  mockInvoke.mockImplementation(async (command: string) => {
    if (command === "get_analytics") return analytics();
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
});
