import { useState } from "react";

import { pauseQueue, resumeQueue, startQueue, stopQueue, toRimaiaError } from "../../lib/commands";
import type { QueueState, RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

type PendingAction = "start" | "pause" | "stop" | null;

interface QueueControlsProps {
  readonly state: QueueState;
  /**
   * Whether this queue has been observed `running` at least once since this
   * component mounted — the only thing that tells "Start" and "Resume" apart.
   * `QueueHandle::resume`'s own doc: "a queue whose state is derived from the
   * database has no way to tell those apart, and no reason to" — the
   * database genuinely cannot distinguish "never started" from "paused
   * again"; wording the button correctly is a courtesy this component adds
   * for its own lifetime, not a fact read off the backend.
   */
  readonly hasRunBefore: boolean;
  /**
   * Whether the queue is supervising a process right now. Stop stays
   * actionable even while `state` is already `paused`, because Pause lets
   * that process finish rather than ending it (ADR-0010) — the switch flips
   * to `paused` immediately, but the run itself can still be in flight.
   */
  readonly hasRunInFlight: boolean;
}

/**
 * Start / Pause / Resume / Stop (task 009's Scope), plus the caption that
 * exists because the difference between the last two is not obvious and
 * getting it wrong at 1am is expensive: **Pause** lets the current run finish
 * and starts nothing new; **Stop** also cancels the run in flight. The
 * caption is always shown, not just conditionally, so the distinction is
 * learned before either button is ever pressed.
 */
export function QueueControls({ state, hasRunBefore, hasRunInFlight }: QueueControlsProps) {
  const [pending, setPending] = useState<PendingAction>(null);
  const [error, setError] = useState<RimaiaError | null>(null);

  function trigger(kind: Exclude<PendingAction, null>, action: () => Promise<void>) {
    setError(null);
    setPending(kind);
    action().then(
      () => setPending(null),
      (thrown) => {
        setError(toRimaiaError(thrown));
        setPending(null);
      },
    );
  }

  const showStop = state === "running" || hasRunInFlight;

  return (
    <div className="queue-controls">
      <div className="queue-controls-buttons">
        {state === "paused" && (
          <button
            type="button"
            disabled={pending !== null}
            onClick={() => trigger("start", hasRunBefore ? resumeQueue : startQueue)}
          >
            {pending === "start" ? "Starting…" : hasRunBefore ? "Resume queue" : "Start queue"}
          </button>
        )}
        {state === "running" && (
          <button
            type="button"
            disabled={pending !== null}
            onClick={() => trigger("pause", pauseQueue)}
          >
            {pending === "pause" ? "Pausing…" : "Pause"}
          </button>
        )}
        {showStop && (
          <button
            type="button"
            className="queue-stop-button"
            disabled={pending !== null}
            onClick={() => trigger("stop", stopQueue)}
          >
            {pending === "stop" ? "Stopping…" : "Stop"}
          </button>
        )}
      </div>
      <p className="queue-controls-caption muted">
        Pause lets the current run finish and starts nothing new. Stop also cancels the run in
        flight.
      </p>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </div>
  );
}
