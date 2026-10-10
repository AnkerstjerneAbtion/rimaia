import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ReviewLoopSection } from "./ReviewLoopSection";
import { taskDetail } from "../../test/reviewFixtures";
import type { ReviewConfig, ReviewLevel, RunCostSummary } from "../../types";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const MEASURED: RunCostSummary = {
  medianUsd: 0.84,
  sampleSize: 12,
  inheritCostUsd: null,
  providerDisplayName: "Claude Code",
};

interface Backend {
  instructions: string | null;
  config: ReviewConfig;
  /** What the level above resolves to: the repository's and global's answer. */
  inherited: ReviewConfig;
  calls: Array<[string, Record<string, unknown> | undefined]>;
}

function install(overrides: Partial<Backend> = {}): Backend {
  const backend: Backend = {
    instructions: null,
    config: {},
    inherited: {
      enabled: "off",
      max_review_loops: 2,
      blocking_severity: "medium",
      fix_session: "fresh",
    },
    calls: [],
    ...overrides,
  };
  mockInvoke.mockImplementation(async (command, args) => {
    backend.calls.push([command, args as Record<string, unknown> | undefined]);
    switch (command) {
      case "get_review_level": {
        const level: ReviewLevel = {
          config: backend.config,
          inherited: backend.inherited,
          effective: { ...backend.inherited, ...backend.config },
        };
        return level;
      }
      case "get_task":
        return taskDetail("task-1", {
          reviewInstructions: backend.instructions,
          reviewConfig: backend.config,
        });
      case "set_task_review": {
        const { reviewInstructions, config } = args as {
          reviewInstructions: string | null;
          config: ReviewConfig;
        };
        backend.instructions = reviewInstructions;
        backend.config = config;
        return { instructions: reviewInstructions, config };
      }
      case "get_strategy_catalogue":
        return {
          catalogue: {
            models: [{ id: "opus", label: "Opus" }],
            efforts: [{ id: "high", label: "High" }],
            planner: { model: "haiku", effort: "low", max_turns: 6 },
          },
          json: "{}",
          defaultJson: "{}",
          providerInfo: { id: "claude-code", displayName: "Claude Code" },
        };
      case "get_run_cost_summary":
        return MEASURED;
      default:
        throw new Error(`unexpected command: ${command}`);
    }
  });
  return backend;
}

function writes(backend: Backend) {
  return backend.calls.filter(([command]) => command === "set_task_review");
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

async function renderLoaded(instructions: string | null = null) {
  render(<ReviewLoopSection taskId="task-1" reviewInstructions={instructions} />);
  return screen.findByRole("group", { name: "Review each task after it is implemented" });
}

describe("the task's review loop section", () => {
  it("says the override replaces the global instructions, in those words", async () => {
    install();
    await renderLoaded();

    expect(
      screen.getByText(
        "Replaces the global review instructions for this task. Leave empty to use them.",
      ),
    ).toBeInTheDocument();
  });

  it("round-trips the task's review instructions, keeping its configuration", async () => {
    const backend = install({ config: { max_review_loops: 3 } });
    await renderLoaded("Run /review.");

    const textarea = screen.getByLabelText("Review instructions");
    expect(textarea).toHaveValue("Run /review.");
    fireEvent.change(textarea, { target: { value: "Run /security-review." } });
    fireEvent.blur(textarea);

    await waitFor(() => expect(writes(backend)).toHaveLength(1));
    expect(writes(backend)[0][1]).toEqual({
      taskId: "task-1",
      reviewInstructions: "Run /security-review.",
      config: { max_review_loops: 3 },
    });
  });

  it("stores a cleared override as none", async () => {
    const backend = install({ instructions: "Run /review." });
    await renderLoaded("Run /review.");

    const textarea = screen.getByLabelText("Review instructions");
    fireEvent.change(textarea, { target: { value: "  " } });
    fireEvent.blur(textarea);

    await waitFor(() => expect(writes(backend)).toHaveLength(1));
    expect(writes(backend)[0][1]).toMatchObject({ reviewInstructions: null });
  });

  it("names the backend's inherited value on every field", async () => {
    install({
      inherited: {
        enabled: "on_cost_acknowledged",
        max_review_loops: 4,
        blocking_severity: "high",
        review_model: "opus",
        fix_session: "resume",
      },
    });
    const group = await renderLoaded();

    expect(within(group).getByLabelText("Inherit (on)")).toBeChecked();
    expect(screen.getByRole("option", { name: "Inherit (4)" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Inherit (high)" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Inherit (opus)" })).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "Inherit (resume the implementation's session)" }),
    ).toBeInTheDocument();
    // No review effort named anywhere above: the task's own strategy.
    expect(screen.getByRole("option", { name: "Inherit (the task's own strategy)" })).toBeInTheDocument();
  });

  describe("turning the loop on", () => {
    it("opens the confirmation and writes nothing for On", async () => {
      const backend = install();
      const group = await renderLoaded();

      fireEvent.click(within(group).getByLabelText("On"));

      const confirm = await screen.findByRole("alertdialog");
      expect(writes(backend)).toHaveLength(0);
      expect(within(group).getByLabelText("Inherit (off)")).toBeChecked();
      expect(confirm).toHaveTextContent("up to about $4.20 more per task");
    });

    it("writes the acknowledgement once, and only on Turn on review loop", async () => {
      const backend = install();
      const group = await renderLoaded();
      fireEvent.click(within(group).getByLabelText("On"));

      fireEvent.click(await screen.findByRole("button", { name: "Turn on review loop" }));

      await waitFor(() => expect(writes(backend)).toHaveLength(1));
      expect(writes(backend)[0][1]).toEqual({
        taskId: "task-1",
        reviewInstructions: null,
        config: { enabled: "on_cost_acknowledged" },
      });
      await waitFor(() => expect(within(group).getByLabelText("On")).toBeChecked());
    });

    it("writes nothing on Cancel and reverts the control to the stored value", async () => {
      const backend = install({ config: { enabled: "off" } });
      const group = await renderLoaded();
      expect(within(group).getByLabelText("Off")).toBeChecked();
      fireEvent.click(within(group).getByLabelText("On"));

      fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

      expect(writes(backend)).toHaveLength(0);
      expect(within(group).getByLabelText("Off")).toBeChecked();
    });

    it("asks again when Inherit would return a task singled out as Off to a loop that is on", async () => {
      const backend = install({
        config: { enabled: "off" },
        inherited: { enabled: "on_cost_acknowledged", max_review_loops: 1 },
      });
      const group = await renderLoaded();

      fireEvent.click(within(group).getByLabelText("Inherit (on)"));

      expect(await screen.findByRole("alertdialog")).toHaveTextContent(
        "With up to 1 fix loop, each task runs up to 3 more sessions",
      );
      expect(writes(backend)).toHaveLength(0);

      fireEvent.click(screen.getByRole("button", { name: "Turn on review loop" }));
      await waitFor(() => expect(writes(backend)).toHaveLength(1));
      expect(writes(backend)[0][1]).toEqual({
        taskId: "task-1",
        reviewInstructions: null,
        config: {},
      });
    });

    it("writes Off at once, and Inherit at once when the level above is off", async () => {
      const backend = install({ config: { enabled: "on_cost_acknowledged" } });
      const group = await renderLoaded();

      fireEvent.click(within(group).getByLabelText("Off"));
      await waitFor(() => expect(writes(backend)).toHaveLength(1));
      expect(screen.queryByRole("alertdialog")).toBeNull();
      expect(writes(backend)[0][1]).toMatchObject({ config: { enabled: "off" } });

      fireEvent.click(await within(group).findByLabelText("Inherit (off)"));
      await waitFor(() => expect(writes(backend)).toHaveLength(2));
      expect(screen.queryByRole("alertdialog")).toBeNull();
      expect(writes(backend)[1][1]).toMatchObject({ config: {} });
    });

    it("uses the effective loop count at this level, not the one above", async () => {
      install({ config: { max_review_loops: 5 } });
      const group = await renderLoaded();

      fireEvent.click(within(group).getByLabelText("On"));

      expect(await screen.findByRole("alertdialog")).toHaveTextContent(
        "up to 11 more sessions",
      );
    });
  });

  it("sets and clears a field back to inherit", async () => {
    const backend = install();
    await renderLoaded();
    const loops = await screen.findByLabelText("Fix loops");

    fireEvent.change(loops, { target: { value: "0" } });
    await waitFor(() => expect(backend.config).toEqual({ max_review_loops: 0 }));
    fireEvent.change(loops, { target: { value: "" } });
    await waitFor(() => expect(backend.config).toEqual({}));
  });
});
