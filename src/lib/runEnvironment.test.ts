import { describe, expect, it } from "vitest";

import { environmentOverheadNote } from "./runEnvironment";
import type { RunCostSummary } from "../types";

const CLAUDE_INHERIT_COST_USD = 0.077;

function summary(overrides: Partial<RunCostSummary>): RunCostSummary {
  return {
    medianUsd: null,
    sampleSize: 0,
    inheritCostUsd: CLAUDE_INHERIT_COST_USD,
    providerDisplayName: "Claude Code",
    ...overrides,
  };
}

describe("environmentOverheadNote", () => {
  it("says nothing at all until a run has reported a cost", () => {
    // The whole point of the note is proportion. With nothing to compare
    // against, the only honest options are silence or the spike's misleading
    // ratio, and this picks silence.
    expect(environmentOverheadNote(null)).toBeNull();
    expect(environmentOverheadNote(summary({ medianUsd: null, sampleSize: 0 }))).toBeNull();
  });

  it("says nothing for a provider nobody has measured", () => {
    // `inheritCostUsd: null` means the provider's own answer, not a gap to
    // fill with someone else's figure.
    expect(
      environmentOverheadNote(
        summary({ medianUsd: 1.0, sampleSize: 4, inheritCostUsd: null }),
      ),
    ).toBeNull();
  });

  it("reports a large share for a cheap run and a small one for an expensive run", () => {
    // Both measured against real runs on the development machine: a $0.12
    // metadata edit and a $2.25 bundle-size investigation. The same fixed cost
    // means very different things to each, which is exactly what quoting a
    // single ratio hid.
    expect(environmentOverheadNote(summary({ medianUsd: 0.12, sampleSize: 9 }))).toContain("64%");
    expect(environmentOverheadNote(summary({ medianUsd: 2.25, sampleSize: 9 }))).toContain("3.4%");
  });

  it("states the cost as a fixed amount, not as a multiple", () => {
    const note = environmentOverheadNote(summary({ medianUsd: 1.0, sampleSize: 4 }));

    expect(note).toContain(`$${CLAUDE_INHERIT_COST_USD.toFixed(2)} of setup per run`);
    expect(note).toContain("once per run, not per turn");
    expect(note).not.toContain("×");
    expect(note).not.toContain("3.6");
  });

  it("states a different provider's own measured cost, not Claude's", () => {
    const note = environmentOverheadNote(
      summary({ medianUsd: 1.0, sampleSize: 4, inheritCostUsd: 0.02 }),
    );

    expect(note).toContain("$0.02 of setup per run");
  });

  it("counts one run in the singular", () => {
    expect(environmentOverheadNote(summary({ medianUsd: 0.5, sampleSize: 1 }))).toContain(
      "across 1 run",
    );
    expect(environmentOverheadNote(summary({ medianUsd: 0.5, sampleSize: 2 }))).toContain(
      "across 2 runs",
    );
  });
});
