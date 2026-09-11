import { useCallback, useEffect, useRef, useState } from "react";

import { cancelPlanPass, planTasksStrategy, toRimaiaError } from "../../lib/commands";
import { subscribeToPlanPassProgress } from "../../lib/events";
import type {
  PlanPass,
  PlanProgress,
  PlanResult,
  PlanSelectionInput,
  RimaiaError,
} from "../../types";

/**
 * The board's planning preflight (task 023): what is being planned right now,
 * and what the pass came to.
 *
 * A planner costs about four cents and four turns; the implementation run it is
 * checking costs on the order of a dollar. Spending forty cents to see how ten
 * cards are modelled, before committing forty dollars, is the best-value check
 * this product offers — and the whole point is that the *user chose to spend
 * it*, which is why the running total is on screen rather than only in the
 * summary.
 *
 * Two states and they are not the same panel: while a pass runs this is a
 * progress readout with a Cancel; once it ends it is a list worth reading
 * before going home — each card, its model and effort, its one-line rationale,
 * and the ones that were skipped with the reason. **An empty column reporting
 * nothing is the failure mode task 023 names**, so a pass that planned nothing
 * still renders every card it passed over.
 */

/** What a skip's stable tag is called on screen. */
const SKIP_LABELS: Record<string, string> = {
  already_proposed: "Already proposed",
  not_planned: "Not planned",
  in_flight: "Already running",
  repository_not_opted_in: "Repository not opted in",
};

function money(usd: number): string {
  return `$${usd.toFixed(2)}`;
}

export interface UsePlanPassResult {
  readonly running: boolean;
  readonly progress: PlanProgress | null;
  readonly pass: PlanPass | null;
  readonly error: RimaiaError | null;
  readonly start: (selection: PlanSelectionInput) => Promise<void>;
  readonly cancel: () => void;
  readonly dismiss: () => void;
}

/**
 * One pass at a time, owned by the board.
 *
 * The subscription is taken for the life of the board rather than for the life
 * of a pass: `subscribeToPlanPassProgress` resolves asynchronously, and a pass
 * started in the same tick as the subscribe would otherwise race it and lose
 * its first card.
 */
export function usePlanPass(): UsePlanPassResult {
  const [running, setRunning] = useState(false);
  const [progress, setProgress] = useState<PlanProgress | null>(null);
  const [pass, setPass] = useState<PlanPass | null>(null);
  const [error, setError] = useState<RimaiaError | null>(null);
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    let unlisten: (() => void) | undefined;
    subscribeToPlanPassProgress((next) => {
      if (live.current) setProgress(next);
    }).then(
      (off) => {
        if (live.current) unlisten = off;
        else off();
      },
      () => {
        // No event bridge — a non-Tauri preview, or a test that never mocks
        // `listen`. The pass still runs and still returns its summary; only
        // the per-card readout is missing.
      },
    );
    return () => {
      live.current = false;
      unlisten?.();
    };
  }, []);

  const start = useCallback(async (selection: PlanSelectionInput) => {
    setRunning(true);
    setProgress(null);
    setPass(null);
    setError(null);
    try {
      setPass(await planTasksStrategy(selection));
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setRunning(false);
      setProgress(null);
    }
  }, []);

  const cancel = useCallback(() => {
    // Deliberately not awaited and deliberately not surfaced: the pass's own
    // promise is what reports the outcome, and a Cancel that arrived a second
    // too late is not a mistake to tell anyone about.
    void cancelPlanPass().catch(() => undefined);
  }, []);

  const dismiss = useCallback(() => {
    setPass(null);
    setError(null);
  }, []);

  return { running, progress, pass, error, start, cancel, dismiss };
}

export function PlanPassPanel({
  running,
  progress,
  pass,
  error,
  onCancel,
  onDismiss,
}: {
  running: boolean;
  progress: PlanProgress | null;
  pass: PlanPass | null;
  error: RimaiaError | null;
  onCancel: () => void;
  onDismiss: () => void;
}) {
  if (!running && !pass && !error) return null;

  return (
    <section className="plan-pass" aria-label="Planning pass">
      {running && (
        <div className="plan-pass-progress" role="status">
          <span className="plan-pass-heading">
            {progress
              ? `Planning ${progress.completed} of ${progress.total}`
              : "Starting the planning pass…"}
          </span>
          {progress && (
            <>
              <span className="plan-pass-current">{progress.result.title}</span>
              <span className="tabular-nums">{money(progress.spentUsd)} spent</span>
            </>
          )}
          <button type="button" onClick={onCancel}>
            Cancel
          </button>
        </div>
      )}

      {error && (
        <p className="plan-pass-error" role="alert">
          {error.message}
        </p>
      )}

      {!running && pass && (
        <>
          <div className="plan-pass-summary">
            <span className="plan-pass-heading">
              {pass.cancelled ? "Planning pass cancelled" : "Planning pass finished"}
            </span>
            <span>
              {pass.planned} planned, {pass.skipped} skipped
            </span>
            <span className="tabular-nums">{money(pass.spentUsd)} spent</span>
            <button type="button" onClick={onDismiss}>
              Dismiss
            </button>
          </div>
          {/* Never a bare "nothing to do": a column with nothing eligible is
              reported card by card, with the reason each one was passed over. */}
          {pass.results.length === 0 ? (
            <p className="muted">
              Nothing in this selection could be planned — no card in it is set to be planned.
            </p>
          ) : (
            <ul className="plan-pass-results">
              {pass.results.map((result) => (
                <PlanPassRow key={result.taskId} result={result} />
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  );
}

function PlanPassRow({ result }: { result: PlanResult }) {
  return (
    <li className={`plan-pass-result plan-pass-result-${result.outcome}`}>
      <span className="plan-pass-result-title">{result.title}</span>
      {result.outcome === "planned" ? (
        <>
          <span className="plan-pass-result-strategy">
            {[result.model, result.effort].filter(Boolean).join(" · ") || "no model named"}
          </span>
          {/* The one line actually worth reading: not what it chose, but why. */}
          {result.rationale && (
            <span className="plan-pass-result-rationale">{result.rationale}</span>
          )}
          {result.costUsd !== null && (
            <span className="plan-pass-result-cost tabular-nums">{money(result.costUsd)}</span>
          )}
        </>
      ) : (
        <span className="plan-pass-result-reason">
          {result.skip ? (SKIP_LABELS[result.skip] ?? result.skip) : "Failed"}
          {result.reason ? ` — ${result.reason}` : ""}
        </span>
      )}
    </li>
  );
}
