import { beforeEach, describe, expect, it, vi } from "vitest";

import { invoke } from "@tauri-apps/api/core";

import {
  acceptTaskStrategy,
  clearTaskStrategy,
  getStrategyApproval,
  getStrategyCatalogue,
  getStrategyDefaults,
  giveUpOnTask,
  planTaskStrategy,
  retryTaskNow,
  setStrategyApproval,
  setStrategyCatalogue,
  setStrategyDefaults,
  toRimaiaError,
} from "./commands";
import type { StrategyCatalogueView, StrategyDefaults } from "../types";

// Mocked at the Tauri seam, not at the wrapper module — the wrappers are what
// is under test here, and mocking them would leave the command names and
// argument keys they send untested. Those names are exactly what
// `scripts/check-command-wiring.sh` cross-checks against `lib.rs`, and the
// argument keys are the half of the contract no script can see.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const mockInvoke = vi.mocked(invoke);

describe("toRimaiaError", () => {
  it("passes a backend {code, message} payload through unchanged", () => {
    const thrown = { code: "not_found", message: "repository not found" };

    expect(toRimaiaError(thrown)).toEqual({
      code: "not_found",
      message: "repository not found",
    });
  });

  it("wraps a bare string rejection as an internal error", () => {
    expect(toRimaiaError("permission denied")).toEqual({
      code: "internal",
      message: "permission denied",
    });
  });

  it("uses a JS Error's message as an internal error", () => {
    expect(toRimaiaError(new Error("network unreachable"))).toEqual({
      code: "internal",
      message: "network unreachable",
    });
  });

  it("renders a non-conforming object as readable JSON, not [object Object]", () => {
    const result = toRimaiaError({ unexpected: "shape" });

    expect(result.code).toBe("internal");
    expect(result.message).not.toBe("[object Object]");
    expect(result.message).toBe('{"unexpected":"shape"}');
  });
});

describe("the execution-strategy wrappers (task 020)", () => {
  const catalogue: StrategyCatalogueView = {
    catalogue: {
      models: [{ id: "opus", label: "Opus" }],
      efforts: [{ id: "low", label: "Low" }],
      planner: { model: "haiku", effort: "low", max_turns: 6 },
    },
    json: '{ "models": [{ "id": "opus", "label": "Opus" }] }',
    defaultJson: '{ "models": [] }',
  };

  const defaults: StrategyDefaults = {
    mode: "manual",
    model: "sonnet",
    effort: "high",
  };

  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("reads the catalogue with no arguments at all", async () => {
    mockInvoke.mockResolvedValue(catalogue);

    await expect(getStrategyCatalogue()).resolves.toEqual(catalogue);
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_catalogue", undefined);
  });

  it("sends an edited catalogue as text and answers with the stored view", async () => {
    // The text is what crosses, not a re-serialized object: the backend stores
    // the user's own key order and indentation, and the view it answers with is
    // what the editor reopens on.
    mockInvoke.mockResolvedValue(catalogue);

    await expect(setStrategyCatalogue(catalogue.json)).resolves.toEqual(catalogue);
    expect(mockInvoke).toHaveBeenCalledWith("set_strategy_catalogue", {
      value: catalogue.json,
    });
  });

  it("asks for the global defaults when no repository is named", async () => {
    mockInvoke.mockResolvedValue(defaults);

    await expect(getStrategyDefaults()).resolves.toEqual(defaults);
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_defaults", {
      repositoryId: null,
    });
  });

  it("asks for one repository's defaults when it is named", async () => {
    mockInvoke.mockResolvedValue(defaults);

    await getStrategyDefaults("repo-1");

    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_defaults", {
      repositoryId: "repo-1",
    });
  });

  it("writes the global defaults and one repository's through the same command", async () => {
    // One command for both levels, because one struct and one precedence chain
    // read both — the repository id is the only thing that differs.
    mockInvoke.mockResolvedValue(undefined);

    await setStrategyDefaults(null, defaults);
    await setStrategyDefaults("repo-1", defaults);

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "set_strategy_defaults", {
      repositoryId: null,
      value: defaults,
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "set_strategy_defaults", {
      repositoryId: "repo-1",
      value: defaults,
    });
  });

  it("reads and writes the approval setting", async () => {
    mockInvoke.mockResolvedValue("manual");

    await expect(getStrategyApproval()).resolves.toBe("manual");
    expect(mockInvoke).toHaveBeenCalledWith("get_strategy_approval", undefined);

    mockInvoke.mockResolvedValue(undefined);
    await setStrategyApproval("manual");

    expect(mockInvoke).toHaveBeenLastCalledWith("set_strategy_approval", {
      value: "manual",
    });
  });

  it("accepts and clears a proposal by task id", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await acceptTaskStrategy("task-1");
    await clearTaskStrategy("task-1");

    expect(mockInvoke).toHaveBeenNthCalledWith(1, "accept_task_strategy", {
      taskId: "task-1",
    });
    expect(mockInvoke).toHaveBeenNthCalledWith(2, "clear_task_strategy", {
      taskId: "task-1",
    });
  });

  it("starts a planner run by task id", async () => {
    mockInvoke.mockResolvedValue(undefined);

    await planTaskStrategy("task-1");

    expect(mockInvoke).toHaveBeenCalledWith("plan_task_strategy", {
      taskId: "task-1",
    });
  });

  it("rejects with a renderable RimaiaError when a strategy command is refused", async () => {
    // Every wrapper goes through the same `call`, so one of them is enough to
    // pin that a refusal arrives as `{code, message}` and not as whatever
    // `invoke` happened to throw.
    mockInvoke.mockRejectedValue({
      code: "invalid",
      message: "the catalogue is not valid JSON: key must be a string at line 1 column 3",
    });

    await expect(setStrategyCatalogue("{ models: [] }")).rejects.toEqual({
      code: "invalid",
      message: "the catalogue is not valid JSON: key must be a string at line 1 column 3",
    });
  });
});

describe("the retry controls (task 014)", () => {
  beforeEach(() => {
    mockInvoke.mockReset();
  });

  it("resumes a waiting task under the name the command is registered as", () => {
    // The name matters more than usual here: `check-command-wiring.sh` reads
    // this literal out of the source to prove the command is registered, so a
    // template string would make the wrapper invisible to it *and* to the user,
    // as a command-not-found at 09:00.
    mockInvoke.mockResolvedValue(undefined);

    return retryTaskNow("task-1").then(() => {
      expect(mockInvoke).toHaveBeenCalledWith("retry_task_now", { taskId: "task-1" });
    });
  });

  it("gives up on a waiting task", () => {
    mockInvoke.mockResolvedValue(undefined);

    return giveUpOnTask("task-1").then(() => {
      expect(mockInvoke).toHaveBeenCalledWith("give_up_on_task", { taskId: "task-1" });
    });
  });

  it("surfaces a refusal as a readable error rather than an object", () => {
    // "Give up" on a task that is not waiting is a sentence about the card, and
    // this is the seam that keeps it one.
    mockInvoke.mockRejectedValue({
      code: "invalid",
      message: "this task is not waiting to be retried (it is running)",
    });

    return giveUpOnTask("task-1").then(
      () => expect.fail("a refusal must reject"),
      (thrown) => {
        const error = toRimaiaError(thrown);
        expect(error.code).toBe("invalid");
        expect(error.message).toContain("not waiting");
      },
    );
  });
});
