import type { RunCostSummary } from "../types";

/**
 * One sentence putting the active provider's own measured setup cost in
 * proportion, or nothing to say yet.
 *
 * The cost itself — `summary.inheritCostUsd` — is a provider's own answer
 * (`AgentProvider::inherit_cost_usd`, task 032), not a constant this module
 * states: quoting Claude's measured figure beside a different provider's
 * toggle would be a fabricated number on the one screen whose whole job is to
 * inform a cost decision. `null` there means say nothing, not "free" or "the
 * spike's number anyway".
 *
 * For the provider that *has* been measured, the spike's headline was "3.6x",
 * and quoting that ratio was misleading in the one direction that matters: it
 * was measured on a one-word prompt where setup *was* the entire run. The cost
 * is charged once per session as cache creation, so it does not scale with the
 * work — the same fixed amount lands on a four-turn run and a forty-turn one.
 * Stating the ratio therefore argues for `strict_local`, which is the opposite
 * of what the spike concluded.
 *
 * So: state the fixed cost, and compare it against runs this installation has
 * actually paid for. Returns null before any run has reported a cost too,
 * where there is nothing honest to compare against.
 */
export function environmentOverheadNote(summary: RunCostSummary | null): string | null {
  if (!summary?.medianUsd || summary.inheritCostUsd == null) return null;

  const cost = summary.inheritCostUsd;
  const share = (cost / summary.medianUsd) * 100;
  const rounded = share >= 10 ? Math.round(share) : Math.round(share * 10) / 10;
  const runs = summary.sampleSize === 1 ? "1 run" : `${summary.sampleSize} runs`;

  return `About $${cost.toFixed(2)} of setup per run — roughly ${rounded}% of your median run so far ($${summary.medianUsd.toFixed(2)} across ${runs}). It is charged once per run, not per turn, so it matters most on short ones.`;
}
