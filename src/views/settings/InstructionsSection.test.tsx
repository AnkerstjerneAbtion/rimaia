import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import { InstructionsSection } from "./InstructionsSection";
import type { TaskSummary } from "../../types";

// `commands.ts` is the only module that imports `invoke`; mocking it here
// exercises the real command-wrapper -> `toRimaiaError` path, same as
// `StorageSection.test.tsx` and `PlanEditor.test.tsx`.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

function taskSummary(overrides: Partial<TaskSummary> = {}): TaskSummary {
  return {
    id: "task-1",
    repositoryId: "repo-1",
    title: "Wire the board",
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
    strategyUpdatedAt: null,
    createdAt: "2026-08-20T11:00:00+00:00",
    updatedAt: "2026-08-20T11:00:00+00:00",
    source: "ui",
    linkCount: 0,
    dependencyCount: 0,
    blockedByIncomplete: false,
    lastRun: null,
    // What a run would spawn with, resolved in Rust and carried on the read
    // (task 020, seam-contract D12's amendment). Nothing configured anywhere
    // is the `claude_code` origin, which is what this fixture's task has.
    effectiveModel: null,
    effectiveEffort: null,
    effectiveOrigin: "claude_code",
    ...overrides,
  };
}

type Handler = (args?: unknown) => unknown;

/** Dispatches every mocked command from one table instead of a per-test
 *  `if`-chain, while keeping the same "unmocked command throws" guarantee
 *  every other test file in this repo relies on to catch a missing mock. */
function mockCommands(overrides: Partial<Record<string, Handler>> = {}) {
  const defaults: Record<string, Handler> = {
    get_base_instructions: () => "Commit as you go, with focused commits.",
    set_base_instructions: () => undefined,
    get_run_environment: () => "inherit",
    set_run_environment: () => undefined,
    list_tasks: () => [taskSummary()],
  };
  const handlers = { ...defaults, ...overrides };
  mockInvoke.mockImplementation(async (command, args) => {
    const handler = handlers[command];
    if (!handler) throw new Error(`unexpected command: ${command}`);
    return handler(args);
  });
}

beforeEach(() => {
  mockInvoke.mockReset();
});

/** Waits for the base-instructions save in flight to settle — the same
 *  pattern `PlanEditor.test.tsx` uses for its own "Saving…" indicator. */
async function waitForInstructionsSaved() {
  await waitFor(() => expect(screen.queryByText("Saving…")).not.toBeInTheDocument());
}

/** Chooses a task in the preview picker once its `<option>` actually exists.
 *
 *  The `<select>` renders immediately with only "Choose a task…" in it — the
 *  real options come from `list_tasks`, which is async. Setting `value` to an
 *  id that has no matching `<option>` is a **silent no-op**: the select keeps
 *  its old value, `onChange` never fires, and no preview is ever requested.
 *  The failure then surfaces as a timeout on the composed text, which points
 *  at the preview rather than at the selection that never happened. Locally
 *  the task list always wins that race; on a slower CI runner it does not. */
async function selectPreviewTask(id: string, title: string) {
  const picker = await screen.findByLabelText("Preview task");
  await screen.findByRole("option", { name: title });
  fireEvent.change(picker, { target: { value: id } });
  return picker;
}

describe("InstructionsSection", () => {
  it("renders the seeded base instructions once get_base_instructions resolves", async () => {
    mockCommands({ get_base_instructions: () => "Commit as you go, with focused commits." });

    render(<InstructionsSection />);

    expect(await screen.findByLabelText("Base instructions")).toHaveValue(
      "Commit as you go, with focused commits.",
    );
  });

  it("saves an edited base instructions value on blur", async () => {
    mockCommands();
    render(<InstructionsSection />);

    const textarea = await screen.findByLabelText("Base instructions");
    fireEvent.change(textarea, { target: { value: "New global rules." } });
    fireEvent.blur(textarea);

    expect(mockInvoke).toHaveBeenCalledWith("set_base_instructions", {
      value: "New global rules.",
    });
    await waitForInstructionsSaved();
  });

  it("does not call set_base_instructions on blur when nothing changed", async () => {
    mockCommands();
    render(<InstructionsSection />);

    const textarea = await screen.findByLabelText("Base instructions");
    fireEvent.blur(textarea);

    expect(mockInvoke).not.toHaveBeenCalledWith("set_base_instructions", expect.anything());
  });

  it("flushes an unsaved base-instructions edit on unmount", async () => {
    mockCommands();
    const { unmount } = render(<InstructionsSection />);

    const textarea = await screen.findByLabelText("Base instructions");
    fireEvent.change(textarea, { target: { value: "typed, never blurred" } });
    unmount();

    expect(mockInvoke).toHaveBeenCalledWith("set_base_instructions", {
      value: "typed, never blurred",
    });
  });

  it("shows the error banner when set_base_instructions rejects", async () => {
    mockCommands();
    mockInvoke.mockImplementation(async (command, args) => {
      if (command === "set_base_instructions") {
        throw { code: "database", message: "settings table is locked" };
      }
      const defaults: Record<string, Handler> = {
        get_base_instructions: () => "original",
        get_run_environment: () => "inherit",
        list_tasks: () => [taskSummary()],
      };
      const handler = defaults[command];
      if (!handler) throw new Error(`unexpected command: ${command}`);
      return handler(args);
    });

    render(<InstructionsSection />);
    const textarea = await screen.findByLabelText("Base instructions");
    fireEvent.change(textarea, { target: { value: "edited" } });
    fireEvent.blur(textarea);

    expect(await screen.findByText("settings table is locked")).toBeInTheDocument();
  });

  it("shows both run-environment options, checked on the stored value, with the trade stated plainly", async () => {
    mockCommands({ get_run_environment: () => "inherit" });

    render(<InstructionsSection />);

    const inherit = await screen.findByRole("radio", { name: "Inherit (default)" });
    const strictLocal = screen.getByRole("radio", { name: "Strict / local" });
    expect(inherit).toBeChecked();
    expect(strictLocal).not.toBeChecked();
    // ADR-0004's amendment says the UI must not soften the cost of
    // inheriting. It used to satisfy that by quoting the spike's "3.6x",
    // which was measured on a one-word prompt where setup *was* the whole
    // run — so it read as "your runs cost 3.6x more", which is false and
    // argues for strict_local, the opposite of what the spike concluded.
    //
    // What must not be softened is the part that does not shrink as a run
    // gets longer: a much larger tool surface, and a personal hook that
    // silently changes how the agent works. The dollar figure is stated in
    // the Runs view, where it can be put against real run costs.
    expect(screen.getByText(/255 tools instead of 26/)).toBeInTheDocument();
    expect(screen.getByText(/SessionStart hook/)).toBeInTheDocument();
    expect(screen.queryByText(/3\.6/)).not.toBeInTheDocument();
  });

  it("switches run environment and calls set_run_environment", async () => {
    mockCommands({ get_run_environment: () => "inherit" });

    render(<InstructionsSection />);
    const strictLocal = await screen.findByRole("radio", { name: "Strict / local" });
    fireEvent.click(strictLocal);

    expect(mockInvoke).toHaveBeenCalledWith("set_run_environment", { value: "strict_local" });
    await waitFor(() => expect(strictLocal).toBeChecked());
  });

  it("reverts the run-environment selection when set_run_environment rejects", async () => {
    mockInvoke.mockImplementation(async (command) => {
      if (command === "get_base_instructions") return "base";
      if (command === "get_run_environment") return "inherit";
      if (command === "list_tasks") return [taskSummary()];
      if (command === "set_run_environment") {
        throw { code: "invalid", message: "cannot change run environment right now" };
      }
      throw new Error(`unexpected command: ${command}`);
    });

    render(<InstructionsSection />);
    const inherit = await screen.findByRole("radio", { name: "Inherit (default)" });
    const strictLocal = screen.getByRole("radio", { name: "Strict / local" });
    fireEvent.click(strictLocal);

    expect(await screen.findByText("cannot change run environment right now")).toBeInTheDocument();
    expect(inherit).toBeChecked();
    expect(strictLocal).not.toBeChecked();
  });

  it("lists tasks in the preview picker and renders the composed prompt as literal preformatted text", async () => {
    mockCommands({
      list_tasks: () => [taskSummary({ id: "task-1", title: "Wire the board" })],
      preview_composed_prompt: () =>
        "# Base instructions\n\nCommit **often**.\n\n# Task\n\nWire the board",
    });

    render(<InstructionsSection />);

    await selectPreviewTask("task-1", "Wire the board");

    expect(mockInvoke).toHaveBeenCalledWith("preview_composed_prompt", { taskId: "task-1" });

    const pre = await screen.findByLabelText("Composed prompt preview");
    expect(pre.tagName).toBe("PRE");
    // Byte for byte, blank lines included — `toHaveTextContent`'s default
    // whitespace normalisation would collapse the very newlines this
    // assertion exists to prove survived, so it is turned off here.
    expect(pre).toHaveTextContent(
      "# Base instructions\n\nCommit **often**.\n\n# Task\n\nWire the board",
      { normalizeWhitespace: false },
    );
    // Not rendered as Markdown: the panel has its own `<h3>`/`<h4>` section
    // headings, but this preview's `# Base instructions` did not add one —
    // react-markdown would render that as an `<h1>`, a level nothing else on
    // this panel uses, and the raw `#`/`**` characters are still literally
    // in the text rather than a `<strong>` swallowing them.
    expect(screen.queryByRole("heading", { level: 1 })).not.toBeInTheDocument();
    expect(screen.queryByText("often")).not.toBeInTheDocument();
    expect(pre.textContent).toContain("**often**");
  });

  it("shows the error banner when preview_composed_prompt rejects", async () => {
    mockCommands({
      preview_composed_prompt: () => {
        throw { code: "not_found", message: "task not found" };
      },
    });

    render(<InstructionsSection />);
    await selectPreviewTask("task-1", "Wire the board");

    expect(await screen.findByText("task not found")).toBeInTheDocument();
  });

  it("ignores a stale preview response that resolves after a later selection's", async () => {
    // Picking task A then quickly B, with A's request resolving last — the
    // order a slow-then-fast pair of real responses can actually arrive in.
    // Whichever selection is current when a response lands must win; an
    // earlier request's answer must never overwrite it.
    const resolvers: Record<string, (value: string) => void> = {};
    mockCommands({
      list_tasks: () => [
        taskSummary({ id: "task-1", title: "Wire the board" }),
        taskSummary({ id: "task-2", title: "Add parser" }),
      ],
      preview_composed_prompt: (args) => {
        const { taskId } = args as { taskId: string };
        return new Promise<string>((resolve) => {
          resolvers[taskId] = resolve;
        });
      },
    });

    render(<InstructionsSection />);
    const picker = await selectPreviewTask("task-1", "Wire the board");

    fireEvent.change(picker, { target: { value: "task-2" } });
    expect(resolvers["task-1"]).toBeDefined();
    expect(resolvers["task-2"]).toBeDefined();

    // The later selection's request resolves first — the order a genuinely
    // slower first request would arrive in.
    await act(async () => resolvers["task-2"]("task 2's composed prompt"));
    await waitFor(() =>
      expect(screen.getByText("task 2's composed prompt")).toBeInTheDocument(),
    );

    await act(async () => resolvers["task-1"]("task 1's composed prompt"));
    expect(screen.queryByText("task 1's composed prompt")).not.toBeInTheDocument();
    expect(screen.getByText("task 2's composed prompt")).toBeInTheDocument();
  });

  it("re-composes the preview after a base-instructions save, for whatever task is already selected", async () => {
    let previewCalls = 0;
    mockCommands({
      preview_composed_prompt: () => {
        previewCalls += 1;
        return previewCalls === 1 ? "first composed prompt" : "second composed prompt";
      },
    });

    render(<InstructionsSection />);
    await selectPreviewTask("task-1", "Wire the board");
    expect(await screen.findByText("first composed prompt")).toBeInTheDocument();

    const textarea = screen.getByLabelText("Base instructions");
    fireEvent.change(textarea, { target: { value: "revised base instructions" } });
    fireEvent.blur(textarea);

    expect(await screen.findByText("second composed prompt")).toBeInTheDocument();
    expect(previewCalls).toBe(2);
  });
});
