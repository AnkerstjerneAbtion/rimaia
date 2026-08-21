import { useEffect, useRef, useState } from "react";

import { cancelRun, getRunTail, getTask, toRimaiaError } from "../../lib/commands";
import { subscribeToRunsTail, subscribeToTasksChanged } from "../../lib/events";
import type { RimaiaError, RunTail, TaskSummary } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

/**
 * `2m 34s`, or `45s` under a minute — a duration, not a point in time, so
 * `lib/board.ts`'s `relativeTime` ("5m ago") is the wrong tool here even
 * though it rounds to the same units. Floors rather than rounds: a run that
 * has been going for 59.9s reads `59s`, not `1m 00s`, which would claim a
 * minute has elapsed before it has.
 */
export function formatElapsed(elapsedMs: number): string {
  const totalSeconds = Math.max(0, Math.floor(elapsedMs / 1000));
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  if (minutes === 0) return `${seconds}s`;
  return `${minutes}m ${seconds.toString().padStart(2, "0")}s`;
}

interface ActiveRunCardProps {
  readonly task: TaskSummary;
  readonly repositoryName: string;
}

/**
 * One currently-running task's live tail (seam-contract D14) plus a Cancel
 * button — task 008's Runs view scope: "elapsed time, turn count, current
 * tool call, and recent assistant text, and a Cancel button."
 *
 * Resolves its own run id from `get_task`: `list_tasks`'s summary carries a
 * last-run *status*, not the row's id (D12's projection is deliberately
 * thin), and there is no `list_active_runs` command (see `commands/runs.rs`'s
 * own header — a run's outcome is read off its task, never a registry). Once
 * the id is known, seeds from `get_run_tail`'s catch-up snapshot — so a
 * client that starts watching mid-run is not blank — then updates from
 * `runs:tail` for as long as a snapshot's `runId` matches. A dropped
 * `runs:tail` message is never replayed (D14 rule 1): the transcript on disk
 * is the record; this is only the live view.
 *
 * `RunsView` keys each card on `task.id` and re-lists rather than remounting
 * on `tasks:changed`, so a one-shot `get_task` that fails — or lands in the
 * narrow window before the `runs` row it names exists — would otherwise
 * never be retried for the card's whole lifetime. This component subscribes
 * to `tasks:changed` itself for exactly that retry, skipping it once `runId`
 * has resolved.
 */
export function ActiveRunCard({ task, repositoryName }: ActiveRunCardProps) {
  const [runId, setRunId] = useState<string | null>(null);
  const [startedAt, setStartedAt] = useState<string | null>(null);
  const [tail, setTail] = useState<RunTail | null>(null);
  // The wall-clock instant a `tail` snapshot was set, so elapsed time can keep
  // ticking between snapshots instead of freezing at whatever the last one
  // said. `RunTail` only advances on an `assistant`/`user`/`result` event
  // (`RunProgress::observe`); a long silent tool call — a six-minute
  // `cargo test`, say — would otherwise leave "Elapsed" reading whatever it
  // said when the tool started for the tool's entire duration, which is the
  // one thing this view exists to let an operator tell apart from a wedged
  // run.
  const [tailObservedAt, setTailObservedAt] = useState<number | null>(null);
  const [cancelling, setCancelling] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [now, setNow] = useState(() => Date.now());
  // Read inside the `tasks:changed` handler below, which is subscribed once
  // per `task.id` rather than re-subscribed on every `runId` change — a ref
  // is what lets that one long-lived handler see the current value instead
  // of the `null` it closed over when it was created.
  const runIdRef = useRef<string | null>(null);

  useEffect(() => {
    const ticker = setInterval(() => setNow(Date.now()), 1_000);
    return () => clearInterval(ticker);
  }, []);

  useEffect(() => {
    runIdRef.current = runId;
  }, [runId]);

  useEffect(() => {
    let active = true;
    setRunId(null);
    setStartedAt(null);
    setTail(null);
    setTailObservedAt(null);
    getTask(task.id).then(
      (detail) => {
        if (active && detail.lastRun) {
          setRunId(detail.lastRun.id);
          setStartedAt(detail.lastRun.startedAt);
        }
      },
      () => {
        // A rejected lookup is not final: the retry effect below tries again
        // on the next relevant `tasks:changed`.
      },
    );
    return () => {
      active = false;
    };
  }, [task.id]);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    // The one-shot lookup above can fail, or land in the narrow window
    // between the task's own `run_state = running` publish and the `runs`
    // row it names actually existing — either way `runId` stays `null` with
    // no retry of its own. Rather than a poll, ride the `tasks:changed` this
    // view's caller (`RunsView`) already subscribes to: every relevant
    // publish is another chance, and one that costs nothing once `runId` has
    // already resolved (the ref guard below skips it).
    subscribeToTasksChanged((taskIds) => {
      if (!active || runIdRef.current !== null) return;
      if (taskIds.length !== 0 && !taskIds.includes(task.id)) return;
      getTask(task.id).then(
        (detail) => {
          if (active && runIdRef.current === null && detail.lastRun) {
            setRunId(detail.lastRun.id);
            setStartedAt(detail.lastRun.startedAt);
          }
        },
        () => {},
      );
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the one-shot
        // lookup above is all this card will ever get.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, [task.id]);

  useEffect(() => {
    if (!runId) return;
    let active = true;
    getRunTail(runId).then(
      (snapshot) => {
        if (active && snapshot) {
          setTail(snapshot);
          setTailObservedAt(Date.now());
        }
      },
      () => {},
    );
    return () => {
      active = false;
    };
  }, [runId]);

  useEffect(() => {
    if (!runId) return;
    let active = true;
    let unlisten: (() => void) | undefined;

    subscribeToRunsTail((snapshot) => {
      if (active && snapshot.runId === runId) {
        setTail(snapshot);
        setTailObservedAt(Date.now());
      }
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the seeded
        // snapshot from `get_run_tail` above is all this card will show.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, [runId]);

  /** Elapsed time as of `now`, ticking between snapshots rather than frozen
   *  on the last one — see `tailObservedAt`'s own doc. Falls back to the
   *  run's own `startedAt` before the first snapshot arrives at all, so the
   *  clock starts the moment a run id is known rather than the moment an
   *  agent first spoke. */
  function elapsedMs(): number {
    if (tail && tailObservedAt !== null) {
      return tail.elapsedMs + Math.max(0, now - tailObservedAt);
    }
    if (startedAt) {
      return Math.max(0, now - new Date(startedAt).getTime());
    }
    return 0;
  }

  function handleCancel() {
    setCancelling(true);
    setError(null);
    cancelRun(task.id).then(
      () => setCancelling(false),
      (thrown) => {
        setError(toRimaiaError(thrown));
        setCancelling(false);
      },
    );
  }

  return (
    <article className="active-run-card">
      <header className="active-run-header">
        <h3>{task.title}</h3>
        <span className="active-run-repo">{repositoryName}</span>
      </header>

      <dl className="detail-list">
        <dt>Elapsed</dt>
        <dd>{startedAt ? formatElapsed(elapsedMs()) : <span className="muted">Starting…</span>}</dd>
        <dt>Turns</dt>
        <dd>{tail ? tail.turns : <span className="muted">—</span>}</dd>
        <dt>Current tool</dt>
        <dd>
          {tail?.currentTool ? (
            <>
              <code>{tail.currentTool.name}</code>
              {tail.currentTool.detail && (
                <span className="muted"> — {tail.currentTool.detail}</span>
              )}
            </>
          ) : (
            <span className="muted">—</span>
          )}
        </dd>
      </dl>

      <div className="active-run-assistant-text">
        <h4>Recent assistant text</h4>
        {tail?.lastAssistantText ? (
          <p>{tail.lastAssistantText}</p>
        ) : (
          <p className="muted">Nothing yet.</p>
        )}
      </div>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <div className="active-run-actions">
        <button type="button" onClick={handleCancel} disabled={cancelling}>
          {cancelling ? "Cancelling…" : "Cancel"}
        </button>
      </div>
    </article>
  );
}
