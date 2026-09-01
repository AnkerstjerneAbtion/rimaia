import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import { StrategySection } from "./StrategySection";
import type {
  EffectiveStrategyFields,
  StrategyCatalogueView,
  StrategyPlan,
  Task,
} from "../../types";

// Mocked at the Tauri seam, not `lib/commands.ts`/`lib/events.ts` — see
// `StorageSection.test.tsx`'s own comment for why. `listen` is mocked because
// this section re-reads the catalogue on `settings:changed`.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);
const mockListen = vi.mocked(listen);

const CATALOGUE: StrategyCatalogueView = {
  catalogue: {
    models: [
      { id: "opus", label: "Opus" },
      { id: "sonnet", label: "Sonnet" },
      { id: "haiku", label: "Haiku" },
    ],
    efforts: [
      { id: "low", label: "Low" },
      { id: "medium", label: "Medium" },
      { id: "high", label: "High" },
    ],
    planner: { model: "haiku", effort: "low", max_turns: 6 },
  },
  json: "{}",
  defaultJson: "{}",
};

const PROPOSAL: StrategyPlan = {
  version: 1,
  status: "proposed",
  model: "sonnet",
  effort: "high",
  workflow: "multi_agent",
  phases: [
    { name: "Schema", model: "sonnet", effort: "medium", agents: 1, summary: "the migration" },
    { name: "Wiring", model: "haiku", effort: "low", agents: 2, summary: "the commands" },
  ],
  rationale: "The plan names a migration and a command surface, which fan out.",
  run: { session_id: "session-1", num_turns: 4, cost_usd: 0.031, error: null },
};

const FAILURE: StrategyPlan = {
  version: 1,
  status: "failed",
  run: {
    session_id: "session-2",
    num_turns: 6,
    cost_usd: 0.004,
    error: "stopped at max_turns without calling set_task_strategy",
  },
};

/** The row `update_task`/`accept_task_strategy`/`clear_task_strategy` answer
 *  with — every one of them returns the task it wrote, and this section
 *  renders the mode off that answer rather than predicting the backend's own
 *  rule (seam-contract D17.6). */
function task(overrides: Partial<Task> = {}): Task {
  return {
    id: "task-1",
    repositoryId: "repo-1",
    title: "Wire up the board",
    plan: "a plan",
    extraInstructions: null,
    column: "ready",
    position: 0,
    runState: "idle",
    branch: null,
    worktreePath: null,
    strategyMode: "default",
    model: null,
    effort: null,
    strategyPlan: null,
    strategySource: null,
    createdAt: "2026-08-20T11:00:00Z",
    updatedAt: "2026-08-20T11:55:00Z",
    strategyUpdatedAt: null,
    source: "ui",
    ...overrides,
  };
}

function effective(overrides: Partial<EffectiveStrategyFields> = {}): EffectiveStrategyFields {
  return {
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
    ...overrides,
  };
}

type SectionProps = Parameters<typeof StrategySection>[0];

function renderSection(overrides: Partial<SectionProps> = {}) {
  const props: SectionProps = {
    taskId: "task-1",
    strategyMode: "default",
    model: null,
    effort: null,
    strategyPlan: null,
    strategySource: null,
    effective: effective(),
    loading: false,
    onChanged: vi.fn(),
    ...overrides,
  };

  const view = render(<StrategySection {...props} />);
  return { ...view, props };
}

/** Waits for the in-flight save to settle — see `PlanEditor.test.tsx`'s
 *  copy of this helper for why. */
async function waitForSaved() {
  await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
}

beforeEach(() => {
  mockInvoke.mockReset();
  mockListen.mockReset();
  mockListen.mockResolvedValue(vi.fn());
  mockInvoke.mockImplementation(async (command) => {
    if (command === "get_strategy_catalogue") return CATALOGUE;
    throw new Error(`unexpected command: ${command}`);
  });
});

describe("StrategySection", () => {
  it("offers every model and effort the catalogue lists, without a copy of the list here", async () => {
    renderSection();

    expect(await screen.findByRole("option", { name: "Sonnet" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Opus" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Medium" })).toBeInTheDocument();
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_catalogue", undefined);
  });

  it("names the effective model, effort and the link of the chain that decided them", async () => {
    renderSection({
      effective: effective({
        effectiveModel: "sonnet",
        effectiveEffort: "high",
        effectiveOrigin: "repository",
      }),
    });

    expect(
      await screen.findByText("Runs as Sonnet · high — from the repository default."),
    ).toBeInTheDocument();
  });

  it("says a run carries no flags at all when nothing is configured anywhere", async () => {
    renderSection({ effective: effective({ effectiveOrigin: "claude_code" }) });

    expect(
      await screen.findByText(
        "Runs with no model or effort flag — Claude Code's own default decides.",
      ),
    ).toBeInTheDocument();
  });

  it("sends the mode as a patch on update_task, so the board and MCP reach one rule", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "update_task") return task({ strategyMode: "planned" });
      throw new Error(`unexpected command: ${command}`);
    });
    const { props } = renderSection();

    fireEvent.change(screen.getByLabelText("Mode"), { target: { value: "planned" } });

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { strategyMode: "planned" },
    });
    await waitForSaved();
    expect(props.onChanged).toHaveBeenCalled();
  });

  it("sends the selected model, and null when switched back to default", async () => {
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "update_task") {
        const patch = (args as { patch: { model: string | null } }).patch;
        return task({ model: patch.model, strategyMode: patch.model ? "manual" : "default" });
      }
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection();
    await screen.findByRole("option", { name: "Sonnet" });

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "opus" } });
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { model: "opus" },
    });
    await waitForSaved();

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "" } });
    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { model: null },
    });
    await waitForSaved();
  });

  it("shows the mode the backend answered with rather than predicting the flip to manual", async () => {
    // D17.6: naming a model flips `strategy_mode` to `manual`. That rule is
    // `tasks::update_task`'s, and this section only renders the row it got
    // back — a second copy of the rule here is exactly what ADR-0006 forbids.
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "update_task") return task({ model: "opus", strategyMode: "manual" });
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ strategyMode: "default" });
    await screen.findByRole("option", { name: "Sonnet" });

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "opus" } });

    await waitFor(() => expect(screen.getByLabelText("Mode")).toHaveValue("manual"));
  });

  it("does not resend the value it was already showing", async () => {
    renderSection({ strategyMode: "manual", model: "sonnet" });
    await screen.findByRole("option", { name: "Sonnet" });
    mockInvoke.mockClear();

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "sonnet" } });

    expect(mockInvoke).not.toHaveBeenCalled();
  });

  it("reverts the select and shows the backend's own refusal when a save is rejected", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "update_task") {
        throw { code: "invalid", message: "a run is already in progress for this task" };
      }
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ strategyMode: "manual", model: "sonnet" });
    await screen.findByRole("option", { name: "Sonnet" });

    fireEvent.change(screen.getByLabelText("Model"), { target: { value: "opus" } });

    expect(
      await screen.findByText("a run is already in progress for this task"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Model")).toHaveValue("sonnet");
  });

  // -------------------------------------------------------------------------
  // The catalogue is configuration, not a closed set (ADR-0016).
  // -------------------------------------------------------------------------

  it("still offers a stored model the catalogue no longer lists, and says so", async () => {
    renderSection({ strategyMode: "manual", model: "sonnet-4-6" });
    await screen.findByRole("option", { name: "Sonnet" });

    expect(screen.getByLabelText("Model")).toHaveValue("sonnet-4-6");
    expect(
      screen.getByText("“sonnet-4-6” is not in the catalogue — a run still passes it verbatim."),
    ).toBeInTheDocument();
  });

  it("keeps a task's own values selectable when the catalogue cannot be read at all", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") throw { code: "database", message: "no" };
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({ strategyMode: "manual", model: "opus" });

    await waitFor(() => expect(screen.getByLabelText("Model")).toHaveValue("opus"));
  });

  // -------------------------------------------------------------------------
  // The mode gates the two choices (ADR-0016).
  // -------------------------------------------------------------------------

  it("leaves the model and effort editable in manual mode", async () => {
    renderSection({ strategyMode: "manual" });
    await screen.findByRole("option", { name: "Sonnet" });

    expect(screen.getByLabelText("Model")).toBeEnabled();
    expect(screen.getByLabelText("Effort")).toBeEnabled();
  });

  it("locks the model and effort in planned mode, naming the planner as their author", async () => {
    renderSection({ strategyMode: "planned", model: "sonnet", effort: "high" });
    await screen.findByRole("option", { name: "Sonnet" });

    expect(screen.getByLabelText("Model")).toBeDisabled();
    expect(screen.getByLabelText("Effort")).toBeDisabled();
    expect(
      screen.getByText("Chosen by the planner. Use Edit below to change them yourself."),
    ).toBeInTheDocument();
  });

  it("unlocks the planner's choices when Edit is pressed", async () => {
    renderSection({
      strategyMode: "planned",
      model: "sonnet",
      effort: "high",
      strategySource: "planner",
      strategyPlan: JSON.stringify(PROPOSAL),
    });
    await screen.findByRole("option", { name: "Sonnet" });

    fireEvent.click(screen.getByRole("button", { name: "Edit" }));

    expect(screen.getByLabelText("Model")).toBeEnabled();
    expect(screen.getByLabelText("Model")).toHaveValue("sonnet");
  });

  // -------------------------------------------------------------------------
  // The planner's proposal (seam-contract D17.3).
  // -------------------------------------------------------------------------

  it("renders the planner's rationale, workflow, phases and its own turns and cost", async () => {
    renderSection({
      strategyMode: "planned",
      model: "sonnet",
      effort: "high",
      strategySource: "planner",
      strategyPlan: JSON.stringify(PROPOSAL),
    });

    expect(
      await screen.findByText("The plan names a migration and a command surface, which fan out."),
    ).toBeInTheDocument();
    expect(screen.getByText("Several agents, in phases")).toBeInTheDocument();
    expect(screen.getByText("Schema")).toBeInTheDocument();
    expect(screen.getByText("Wiring")).toBeInTheDocument();
    expect(screen.getByText("— the migration")).toBeInTheDocument();
    // The strategy run gets no `runs` row (D17.5), so this is only readable
    // from inside the envelope.
    expect(screen.getByText(/4 turns · \$0\.0310/)).toBeInTheDocument();
  });

  it("takes authorship of a proposal, and reads as accepted once the row says so", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "accept_task_strategy") {
        return task({ strategyMode: "planned", strategySource: "user", model: "sonnet" });
      }
      throw new Error(`unexpected command: ${command}`);
    });
    const proposed = {
      strategyMode: "planned" as const,
      model: "sonnet",
      effort: "high",
      strategyPlan: JSON.stringify(PROPOSAL),
    };
    const { rerender, props } = renderSection({ ...proposed, strategySource: "planner" });
    await screen.findByText("Proposed by the planner — not accepted yet.");

    fireEvent.click(screen.getByRole("button", { name: "Accept" }));

    expect(mockInvoke).toHaveBeenCalledWith("accept_task_strategy", { taskId: "task-1" });
    await waitFor(() => expect(props.onChanged).toHaveBeenCalled());

    // "Accepted" is `strategy_source` flipping `planner` → `user` (D17.7);
    // the flip arrives as a prop, the way every other external write to this
    // row does.
    rerender(<StrategySection {...props} {...proposed} strategySource="user" />);
    expect(screen.getByText("Accepted — this strategy is yours now.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Accept" })).toBeNull();
  });

  it("takes the task off the planner's hands when Override is pressed", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "update_task") return task({ strategyMode: "manual", model: "sonnet" });
      throw new Error(`unexpected command: ${command}`);
    });
    renderSection({
      strategyMode: "planned",
      model: "sonnet",
      strategySource: "planner",
      strategyPlan: JSON.stringify(PROPOSAL),
    });
    await screen.findByRole("button", { name: "Override" });

    fireEvent.click(screen.getByRole("button", { name: "Override" }));

    expect(mockInvoke).toHaveBeenCalledWith("update_task", {
      id: "task-1",
      patch: { strategyMode: "manual" },
    });
    await waitFor(() => expect(screen.getByLabelText("Mode")).toHaveValue("manual"));
  });

  it("clears the recorded proposal on Re-plan, which is the only thing that lifts the guard", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "clear_task_strategy") return task({ strategyMode: "planned" });
      throw new Error(`unexpected command: ${command}`);
    });
    const { props } = renderSection({
      strategyMode: "planned",
      strategySource: "planner",
      strategyPlan: JSON.stringify(PROPOSAL),
    });
    await screen.findByRole("button", { name: "Re-plan" });

    fireEvent.click(screen.getByRole("button", { name: "Re-plan" }));

    expect(mockInvoke).toHaveBeenCalledWith("clear_task_strategy", { taskId: "task-1" });
    await waitFor(() => expect(props.onChanged).toHaveBeenCalled());
  });

  it("ignores a strategy_plan that is not a readable envelope rather than taking the panel down", async () => {
    renderSection({ strategyMode: "planned", strategyPlan: "{ not json" });

    // The same tolerance the backend applies to a hand-edited settings row:
    // unreadable reads as "no proposal recorded".
    expect(
      await screen.findByText(
        "The planner runs once, before this task's next run, and proposes a model and effort here.",
      ),
    ).toBeInTheDocument();
  });

  // -------------------------------------------------------------------------
  // A planner failure is annotated, never fatal (§3 of task 020's plan).
  // -------------------------------------------------------------------------

  it("reads back the planner's own failure and says what the task runs on instead", async () => {
    renderSection({
      strategyMode: "planned",
      strategySource: "planner",
      strategyPlan: JSON.stringify(FAILURE),
    });

    expect(
      await screen.findByText(
        "The planner did not produce a strategy (stopped at max_turns without calling set_task_strategy). This task runs on the default strategy.",
      ),
    ).toBeInTheDocument();
  });

  it("re-runs the planner from the failure state", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_strategy_catalogue") return CATALOGUE;
      if (command === "plan_task_strategy") return undefined;
      throw new Error(`unexpected command: ${command}`);
    });
    const { props } = renderSection({
      strategyMode: "planned",
      strategySource: "planner",
      strategyPlan: JSON.stringify(FAILURE),
    });

    fireEvent.click(await screen.findByRole("button", { name: "Plan now" }));

    expect(mockInvoke).toHaveBeenCalledWith("plan_task_strategy", { taskId: "task-1" });
    await waitFor(() => expect(props.onChanged).toHaveBeenCalled());
  });

  it("names a failure with no recorded reason rather than rendering an empty parenthesis", async () => {
    renderSection({
      strategyMode: "planned",
      strategyPlan: JSON.stringify({ version: 1, status: "failed" }),
    });

    expect(
      await screen.findByText(
        "The planner did not produce a strategy (no reason recorded). This task runs on the default strategy.",
      ),
    ).toBeInTheDocument();
  });
});
