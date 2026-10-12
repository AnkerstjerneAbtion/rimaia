import { describe, expect, it } from "vitest";

import type { ReviewConfig, ReviewLevel } from "../types";
import {
  MAX_REVIEW_LOOP_OPTIONS,
  enabledOutcome,
  globalEnabledOutcome,
  inheritedValueText,
  maxReviewLoopsLabel,
  withField,
  withoutField,
} from "./reviewConfig";

function level(config: ReviewConfig, inheritedEnabled: "off" | "on_cost_acknowledged"): ReviewLevel {
  const inherited: ReviewConfig = {
    enabled: inheritedEnabled,
    max_review_loops: 2,
    blocking_severity: "medium",
    fix_session: "fresh",
  };
  return { config, inherited, effective: { ...inherited, ...config } };
}

describe("enabledOutcome", () => {
  it("opens the acknowledgement, and writes nothing, for On", () => {
    expect(enabledOutcome("on", level({}, "off"))).toEqual({
      kind: "acknowledge",
      config: { enabled: "on_cost_acknowledged" },
    });
  });

  it("opens the acknowledgement for Inherit when the level above is on", () => {
    // A task singled out as Off under a repository that is on.
    expect(enabledOutcome("inherit", level({ enabled: "off" }, "on_cost_acknowledged"))).toEqual({
      kind: "acknowledge",
      config: {},
    });
  });

  it("writes at once for Off, and for Inherit when the level above is off", () => {
    expect(enabledOutcome("off", level({}, "on_cost_acknowledged"))).toEqual({
      kind: "write",
      config: { enabled: "off" },
    });
    expect(
      enabledOutcome("inherit", level({ enabled: "on_cost_acknowledged" }, "off")),
    ).toEqual({ kind: "write", config: {} });
  });

  it("does nothing for the choice that is already stored", () => {
    expect(enabledOutcome("inherit", level({}, "off")).kind).toBe("none");
    expect(enabledOutcome("off", level({ enabled: "off" }, "on_cost_acknowledged")).kind).toBe(
      "none",
    );
    expect(
      enabledOutcome("on", level({ enabled: "on_cost_acknowledged" }, "off")).kind,
    ).toBe("none");
  });

  it("keeps every other field when it changes enabled", () => {
    const outcome = enabledOutcome("on", level({ max_review_loops: 4 }, "off"));
    expect(outcome).toEqual({
      kind: "acknowledge",
      config: { max_review_loops: 4, enabled: "on_cost_acknowledged" },
    });
  });

  it("never produces any spelling of on but the acknowledgement", () => {
    for (const choice of ["inherit", "off", "on"] as const) {
      for (const inherited of ["off", "on_cost_acknowledged"] as const) {
        const outcome = enabledOutcome(choice, level({}, inherited));
        if (outcome.kind === "none") continue;
        expect([undefined, "off", "on_cost_acknowledged"]).toContain(outcome.config.enabled);
      }
    }
  });
});

describe("globalEnabledOutcome", () => {
  it("acknowledges when checked and writes off at once when unchecked", () => {
    expect(globalEnabledOutcome(true, level({}, "off"))).toEqual({
      kind: "acknowledge",
      config: { enabled: "on_cost_acknowledged" },
    });
    expect(
      globalEnabledOutcome(false, level({ enabled: "on_cost_acknowledged" }, "off")),
    ).toEqual({ kind: "write", config: { enabled: "off" } });
  });

  it("does nothing when the box already says so", () => {
    expect(globalEnabledOutcome(false, level({}, "off")).kind).toBe("none");
    expect(globalEnabledOutcome(true, level({ enabled: "on_cost_acknowledged" }, "off")).kind).toBe(
      "none",
    );
  });
});

describe("the field helpers", () => {
  it("clears and sets one field without touching the rest", () => {
    const config: ReviewConfig = { max_review_loops: 3, fix_session: "resume" };
    expect(withoutField(config, "max_review_loops")).toEqual({ fix_session: "resume" });
    expect(withField(config, "blocking_severity", "high")).toEqual({
      max_review_loops: 3,
      fix_session: "resume",
      blocking_severity: "high",
    });
    expect(withField(config, "fix_session", undefined)).toEqual({ max_review_loops: 3 });
  });

  it("offers exactly 0 to 5 loops, and labels 0 as review only", () => {
    expect([...MAX_REVIEW_LOOP_OPTIONS]).toEqual([0, 1, 2, 3, 4, 5]);
    expect(maxReviewLoopsLabel(0)).toBe("0 — Review only, no fixes");
    expect(maxReviewLoopsLabel(3)).toBe("3");
  });

  it("words the inherited value from the backend's answer", () => {
    const inherited: ReviewConfig = {
      enabled: "on_cost_acknowledged",
      max_review_loops: 4,
      blocking_severity: "high",
      fix_session: "resume",
    };
    expect(inheritedValueText("enabled", inherited)).toBe("on");
    expect(inheritedValueText("max_review_loops", inherited)).toBe("4");
    expect(inheritedValueText("blocking_severity", inherited)).toBe("high");
    expect(inheritedValueText("review_model", inherited)).toBe("the task's own strategy");
    expect(inheritedValueText("review_model", { review_model: "opus" })).toBe("opus");
    expect(inheritedValueText("fix_session", inherited)).toBe(
      "resume the implementation's session",
    );
    expect(inheritedValueText("enabled", { enabled: "off" })).toBe("off");
  });
});
