import type { RunCostSummary } from "../types";

/**
 * What inheriting the operator's environment adds to a run, in dollars.
 *
 * Mirrors `ENVIRONMENT_SETUP_COST_USD` in `crates/core/src/db/settings.rs`,
 * whose doc comment carries the measurement and the argument. Kept here rather
 * than fetched because it is a property of the spike, not of this machine —
 * what *is* per-machine is the run cost it gets compared against.
 */
export const ENVIRONMENT_SETUP_COST_USD = 0.077;

/**
 * One sentence putting that fixed cost in proportion, or nothing to say yet.
 *
 * The spike's headline was "3.6x", and quoting that ratio was misleading in the
 * one direction that matters: it was measured on a one-word prompt where setup
 * *was* the entire run. The cost is charged once per session as cache creation,
 * so it does not scale with the work — the same ~$0.08 lands on a four-turn run
 * and a forty-turn one. Stating the ratio therefore argues for `strict_local`,
 * which is the opposite of what the spike concluded.
 *
 * So: state the fixed cost, and compare it against runs this installation has
 * actually paid for. Returns null before any run has reported a cost, where
 * there is nothing honest to compare against.
 */
export function environmentOverheadNote(summary: RunCostSummary | null): string | null {
  if (!summary?.medianUsd) return null;

  const share = (ENVIRONMENT_SETUP_COST_USD / summary.medianUsd) * 100;
  const rounded = share >= 10 ? Math.round(share) : Math.round(share * 10) / 10;
  const runs = summary.sampleSize === 1 ? "1 run" : `${summary.sampleSize} runs`;

  return `About $${ENVIRONMENT_SETUP_COST_USD.toFixed(2)} of setup per run — roughly ${rounded}% of your median run so far ($${summary.medianUsd.toFixed(2)} across ${runs}). It is charged once per run, not per turn, so it matters most on short ones.`;
}
