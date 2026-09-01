import { describe, expect, it } from "vitest";

import { ENVIRONMENT_SETUP_COST_USD, environmentOverheadNote } from "./runEnvironment";

describe("environmentOverheadNote", () => {
  it("says nothing at all until a run has reported a cost", () => {
    // The whole point of the note is proportion. With nothing to compare
    // against, the only honest options are silence or the spike's misleading
    // ratio, and this picks silence.
    expect(environmentOverheadNote(null)).toBeNull();
    expect(environmentOverheadNote({ medianUsd: null, sampleSize: 0 })).toBeNull();
  });

  it("reports a large share for a cheap run and a small one for an expensive run", () => {
    // Both measured against real runs on the development machine: a $0.12
    // metadata edit and a $2.25 bundle-size investigation. The same fixed cost
    // means very different things to each, which is exactly what quoting a
    // single ratio hid.
    expect(environmentOverheadNote({ medianUsd: 0.12, sampleSize: 9 })).toContain("64%");
    expect(environmentOverheadNote({ medianUsd: 2.25, sampleSize: 9 })).toContain("3.4%");
  });

  it("states the cost as a fixed amount, not as a multiple", () => {
    const note = environmentOverheadNote({ medianUsd: 1.0, sampleSize: 4 });

    expect(note).toContain(`$${ENVIRONMENT_SETUP_COST_USD.toFixed(2)} of setup per run`);
    expect(note).toContain("once per run, not per turn");
    expect(note).not.toContain("×");
    expect(note).not.toContain("3.6");
  });

  it("counts one run in the singular", () => {
    expect(environmentOverheadNote({ medianUsd: 0.5, sampleSize: 1 })).toContain("across 1 run");
    expect(environmentOverheadNote({ medianUsd: 0.5, sampleSize: 2 })).toContain("across 2 runs");
  });
});
