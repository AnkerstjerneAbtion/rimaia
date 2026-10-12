import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { ReviewSection } from "./ReviewSection";
import type { ReviewConfig, ReviewLevel, RunCostSummary } from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts`, so each assertion is about
// the exact command and arguments a click sent.
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const DEFAULTS: ReviewConfig = {
  enabled: "off",
  max_review_loops: 2,
  blocking_severity: "medium",
  fix_session: "fresh",
};

const MEASURED: RunCostSummary = {
  medianUsd: 0.84,
  sampleSize: 12,
  inheritCostUsd: null,
  providerDisplayName: "Claude Code",
};

interface Backend {
  settings: { instructions: string; config: ReviewConfig };
  calls: Array<[string, Record<string, unknown> | undefined]>;
}

function install(initial: Partial<Backend["settings"]> = {}): Backend {
  const backend: Backend = {
    settings: { instructions: "", config: {}, ...initial },
    calls: [],
  };
  mockInvoke.mockImplementation(async (command, args) => {
    backend.calls.push([command, args as Record<string, unknown> | undefined]);
    switch (command) {
      case "get_review_settings":
        return backend.settings;
      case "set_review_settings": {
        const { instructions, config } = args as { instructions: string; config: ReviewConfig };
        backend.settings = { instructions, config };
        return backend.settings;
      }
      case "get_review_level": {
        const config = backend.settings.config;
        const level: ReviewLevel = {
          config,
          inherited: DEFAULTS,
          effective: { ...DEFAULTS, ...config },
        };
        return level;
      }
      case "get_strategy_catalogue":
        return {
          catalogue: {
            models: [
              { id: "opus", label: "Opus" },
              { id: "sonnet", label: "Sonnet" },
            ],
            efforts: [
              { id: "low", label: "Low" },
              { id: "high", label: "High" },
            ],
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
  return backend.calls.filter(([command]) => command === "set_review_settings");
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
});

async function renderLoaded() {
  render(<ReviewSection />);
  return screen.findByRole("checkbox", { name: "Review each task after it is implemented" });
}

describe("Settings → Review", () => {
  it("round-trips the global review instructions", async () => {
    const backend = install({ instructions: "Run /review." });
    render(<ReviewSection />);

    const textarea = await screen.findByRole("textbox", { name: "Review instructions" });
    expect(textarea).toHaveValue("Run /review.");
    fireEvent.change(textarea, { target: { value: "Run /security-review." } });
    fireEvent.blur(textarea);

    await waitFor(() => expect(writes(backend)).toHaveLength(1));
    expect(writes(backend)[0][1]).toEqual({
      instructions: "Run /security-review.",
      config: {},
    });
  });

  it("opens the cost acknowledgement, and writes nothing, when the box is checked", async () => {
    const backend = install();
    const box = await renderLoaded();

    fireEvent.click(box);

    const confirm = await screen.findByRole("alertdialog");
    expect(writes(backend)).toHaveLength(0);
    expect(box).not.toBeChecked();
    expect(
      within(confirm).getByText(
        "With up to 2 fix loops, each task runs up to 5 more sessions: a review, then a fix and another review per loop. At your median run so far ($0.84 across 12 runs), that is up to about $4.20 more per task.",
      ),
    ).toBeInTheDocument();
  });

  it("writes the acknowledgement, exactly once, when Turn on review loop is pressed", async () => {
    const backend = install({ instructions: "Run /review." });
    const box = await renderLoaded();
    fireEvent.click(box);

    fireEvent.click(await screen.findByRole("button", { name: "Turn on review loop" }));

    await waitFor(() => expect(writes(backend)).toHaveLength(1));
    expect(writes(backend)[0][1]).toEqual({
      instructions: "Run /review.",
      config: { enabled: "on_cost_acknowledged" },
    });
    await waitFor(() => expect(box).toBeChecked());
    expect(screen.queryByRole("alertdialog")).toBeNull();
  });

  it("writes nothing when Cancel is pressed, and the box keeps showing what is stored", async () => {
    const backend = install();
    const box = await renderLoaded();
    fireEvent.click(box);

    fireEvent.click(await screen.findByRole("button", { name: "Cancel" }));

    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(writes(backend)).toHaveLength(0);
    expect(box).not.toBeChecked();
  });

  it("turns the loop off at once, with no confirmation", async () => {
    const backend = install({ config: { enabled: "on_cost_acknowledged" } });
    const box = await renderLoaded();
    await waitFor(() => expect(box).toBeChecked());

    fireEvent.click(box);

    await waitFor(() => expect(writes(backend)).toHaveLength(1));
    expect(screen.queryByRole("alertdialog")).toBeNull();
    expect(writes(backend)[0][1]).toEqual({ instructions: "", config: { enabled: "off" } });
  });

  it("prices the sentence with the max loops that will be effective", async () => {
    install({ config: { max_review_loops: 0 } });
    const box = await renderLoaded();

    fireEvent.click(box);

    expect(
      await screen.findByText(
        "With no fix loops, each task runs 1 more session: a review that reports findings and fixes nothing. At your median run so far ($0.84 across 12 runs), that is about $0.84 more per task.",
      ),
    ).toBeInTheDocument();
  });

  it("offers exactly 0 to 5 fix loops and the built-in default", async () => {
    install();
    await renderLoaded();

    const select = (await screen.findByLabelText("Fix loops")) as HTMLSelectElement;

    expect(Array.from(select.options).map((option) => option.textContent)).toEqual([
      "Built-in default (2)",
      "0 — Review only, no fixes",
      "1",
      "2",
      "3",
      "4",
      "5",
    ]);
  });

  it("sets every field, and clears it back to the built-in default", async () => {
    const backend = install();
    await renderLoaded();

    const change = async (label: string, value: string) => {
      const select = await screen.findByLabelText(label);
      fireEvent.change(select, { target: { value } });
      await waitFor(() => expect(select).toHaveValue(value));
    };
    await change("Fix loops", "4");
    await change("Blocking severity", "high");
    await change("Review model", "opus");
    await change("Review effort", "low");
    await change("Fix session", "resume");

    expect(backend.settings.config).toEqual({
      max_review_loops: 4,
      blocking_severity: "high",
      review_model: "opus",
      review_effort: "low",
      fix_session: "resume",
    });

    await change("Fix loops", "");
    await change("Blocking severity", "");
    await change("Review model", "");
    await change("Review effort", "");
    await change("Fix session", "");
    expect(backend.settings.config).toEqual({});
  });

  it("never sends any spelling of on but the acknowledgement", async () => {
    const backend = install();
    const box = await renderLoaded();
    fireEvent.click(box);
    fireEvent.click(await screen.findByRole("button", { name: "Turn on review loop" }));
    await waitFor(() => expect(box).toBeChecked());
    fireEvent.click(box);
    await waitFor(() => expect(box).not.toBeChecked());

    for (const [, args] of writes(backend)) {
      const config = (args as { config: ReviewConfig }).config;
      expect([undefined, "off", "on_cost_acknowledged"]).toContain(config.enabled);
    }
    expect(JSON.stringify(writes(backend))).not.toMatch(/"enabled":(true|"on")/);
  });

  it("shows what the backend kept when another writer changed it", async () => {
    const backend = install();
    const box = await renderLoaded();
    expect(box).not.toBeChecked();

    backend.settings = { instructions: "", config: { enabled: "on_cost_acknowledged" } };
    const settingsChanged = mockListen.mock.calls.find(([name]) => name === "settings:changed");
    (settingsChanged?.[1] as (event: { payload: null }) => void)({ payload: null });

    await waitFor(() => expect(box).toBeChecked());
  });
});
