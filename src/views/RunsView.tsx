import { useEffect, useRef, useState } from "react";

import { ActiveRunCard } from "../components/runs/ActiveRunCard";
import { QueueControls } from "../components/runs/QueueControls";
import { QueuePlanList } from "../components/runs/QueuePlanList";
import { SessionOutcomesList } from "../components/runs/SessionOutcomesList";
import type { RunCostSummary } from "../types";
import type { SessionOutcome } from "../components/runs/SessionOutcomesList";
import { EmptyState } from "../components/EmptyState";
import { environmentOverheadNote } from "../lib/runEnvironment";
import { ErrorBanner } from "../components/ErrorBanner";
import {
  getQueueStatus,
  getRunCostSummary,
  getRunEnvironment,
  getTask,
  listRepositories,
  listTasks,
  toRimaiaError,
} from "../lib/commands";
import {
  subscribeToRunsChanged,
  subscribeToSettingsChanged,
  subscribeToTasksChanged,
} from "../lib/events";
import type { QueueStatus, RimaiaError, RunEnvironment, TaskSummary } from "../types";

/**
 * Task 008's Runs view: whatever task is running right now, live, with a
 * Cancel button — the "riskiest component" ADR-0014 names, made visible.
 * Task 009 adds the queue around it: whether it is running or paused, the
 * ordered list of what is next, Start/Pause/Resume/Stop, and what has
 * finished this session.
 *
 * There is no `list_active_runs` command (`commands/runs.rs`'s own header:
 * a run's outcome is read off its task, not a registry) — "what is running"
 * is `list_tasks({ runState: "running" })`, the same filter the board's own
 * toolbar already exposes as a first-class query, refreshed on every
 * `tasks:changed` the same way every other view in this app stays live.
 * Each returned task's own live tail is `ActiveRunCard`'s concern, not this
 * one's — this view only decides *which* tasks get a card.
 */
export function RunsView() {
  const [runningTasks, setRunningTasks] = useState<TaskSummary[] | null>(null);
  const [readError, setReadError] = useState<RimaiaError | null>(null);
  const [repositoryNames, setRepositoryNames] = useState<ReadonlyMap<string, string>>(new Map());
  const [runEnvironment, setRunEnvironment] = useState<RunEnvironment | null>(null);
  const [runCosts, setRunCosts] = useState<RunCostSummary | null>(null);

  const [queueStatus, setQueueStatus] = useState<QueueStatus | null>(null);
  const [queueError, setQueueError] = useState<RimaiaError | null>(null);
  const [hasRunBefore, setHasRunBefore] = useState(false);
  const [sessionOutcomes, setSessionOutcomes] = useState<SessionOutcome[]>([]);

  // Read inside the queue-status effect below, which subscribes once rather
  // than on every `repositoryNames` change — same reason `ActiveRunCard`'s
  // own `runIdRef` exists: a ref lets a long-lived closure see the current
  // value instead of the one it closed over when it was created.
  const repositoryNamesRef = useRef(repositoryNames);
  useEffect(() => {
    repositoryNamesRef.current = repositoryNames;
  }, [repositoryNames]);

  // Cosmetic context, fetched once: a repository name for each card's header,
  // and the environment mode ADR-0004's amendment asks to keep "within reach"
  // of the per-run cost — which is what a finished run's own outcome section
  // shows plainly (`RunOutcomeSection`, task detail), since `total_cost_usd`
  // does not exist until a run ends. Neither failure here blocks the live
  // tail below: a card falls back to the raw repository id, same as the
  // board does, and the note simply does not render.
  useEffect(() => {
    listRepositories().then(
      (repositories) => setRepositoryNames(new Map(repositories.map((r) => [r.id, r.name]))),
      () => {},
    );
    getRunEnvironment().then(setRunEnvironment, () => {});
    // Cosmetic, like the two above: a failure here costs the proportion in the
    // note below and nothing else, so it stays silent rather than raising a
    // banner over the queue.
    getRunCostSummary().then(setRunCosts, () => {});
  }, []);

  useEffect(() => {
    let active = true;

    function refresh() {
      listTasks({ runState: "running" }).then(
        (tasks) => {
          if (active) {
            setRunningTasks(tasks);
            setReadError(null);
          }
        },
        (thrown) => {
          if (active) setReadError(toRimaiaError(thrown));
        },
      );
    }

    refresh();

    let unlisten: (() => void) | undefined;
    subscribeToTasksChanged(() => {
      if (active) refresh();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the initial
        // `refresh()` above is all this view will ever show.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  // The queue's own status (ADR-0010), re-read on every mutation that could
  // change it: a task moving or ending (`tasks:changed`/`runs:changed`) or
  // the switch itself (`queue_state` lives in `settings` — `settings:changed`).
  // `getQueueStatus` re-reads fresh every time (its own doc comment), so a
  // card dragged to the top mid-queue shows up here as what runs next before
  // the queue itself gets there.
  useEffect(() => {
    let active = true;

    // Ids of runs already turned into a `SessionOutcome` — two events landing
    // close together (a run's own `runs:changed` and the board's
    // `tasks:changed`) must not list the same finished run twice.
    const resolvedRunIds = new Set<string>();
    // `undefined` means "no read yet": the very first fetch has nothing to
    // compare against, only a starting point to record. `null` means the
    // queue was idle at the last read.
    let previousRunningTaskId: string | null | undefined;

    function noteCompletion(runningTaskId: string | null) {
      const previous = previousRunningTaskId;
      previousRunningTaskId = runningTaskId;
      if (previous === undefined || previous === null || previous === runningTaskId) return;

      // `previous` is no longer the queue's in-flight task, so whatever it
      // was doing has ended — its own `lastRun` is the only source of truth
      // for how (D14: the live tail is never that source).
      getTask(previous).then(
        (detail) => {
          if (!active || !detail.lastRun || resolvedRunIds.has(detail.lastRun.id)) return;
          resolvedRunIds.add(detail.lastRun.id);
          const finished = detail.lastRun;
          setSessionOutcomes((outcomes) =>
            [
              {
                taskId: detail.id,
                title: detail.title,
                repositoryName:
                  repositoryNamesRef.current.get(detail.repositoryId) ?? detail.repositoryId,
                status: finished.status,
                exitClass: finished.exitClass,
                endedAt: finished.endedAt ?? new Date().toISOString(),
              },
              ...outcomes,
            ].slice(0, 20),
          );
        },
        () => {
          // The task may since have been deleted, or the read raced a
          // further mutation. Nothing to show, and nothing to retry: the
          // next transition this detector sees is unrelated to this one.
        },
      );
    }

    function refresh() {
      getQueueStatus().then(
        (status) => {
          if (!active) return;
          setQueueStatus(status);
          setQueueError(null);
          if (status.state === "running") setHasRunBefore(true);
          noteCompletion(status.runningTaskId);
        },
        (thrown) => {
          if (active) setQueueError(toRimaiaError(thrown));
        },
      );
    }

    refresh();

    // Three sources can each change what the queue would report — a task
    // moving, a run ending, or the switch itself — and all three just mean
    // "read it again", so every subscription below shares one `settle`
    // rather than each keeping its own `unlisten` slot.
    const unlistens: Array<() => void> = [];
    function settle(subscription: Promise<() => void>) {
      subscription.then(
        (fn) => {
          if (active) {
            unlistens.push(fn);
          } else {
            fn();
          }
        },
        () => {
          // No event bridge (tests, or a non-Tauri preview) — the initial
          // `refresh()` above is all this view will ever show.
        },
      );
    }
    settle(subscribeToTasksChanged(() => active && refresh()));
    settle(subscribeToRunsChanged(() => active && refresh()));
    settle(subscribeToSettingsChanged(() => active && refresh()));

    return () => {
      active = false;
      for (const unlisten of unlistens) unlisten();
    };
  }, []);

  const overheadNote = environmentOverheadNote(runCosts);

  return (
    <div className="runs-view">
      {readError && <ErrorBanner error={readError} onDismiss={() => setReadError(null)} />}
      {queueError && <ErrorBanner error={queueError} onDismiss={() => setQueueError(null)} />}

      <section className="queue-section">
        <div className="queue-section-header">
          <h2>Run queue</h2>
          {queueStatus && (
            <span className={`queue-state-badge queue-state-${queueStatus.state}`}>
              {queueStatus.state === "running" ? "Running" : "Paused"}
            </span>
          )}
        </div>

        {/* The one failure `SkipReason` cannot name: a missing `claude` fails
            `probe_cli` before any task is even chosen, so nothing on the
            board explains it and the state badge above would otherwise read
            "Running" over a full plan while nothing happens all night. */}
        {queueStatus?.lastStepError && (
          <p className="queue-step-error" role="alert">
            The queue could not complete its last pass: {queueStatus.lastStepError}
          </p>
        )}

        {queueStatus === null && !queueError && <p className="muted">Reading queue status…</p>}

        {queueStatus && (
          <>
            <QueueControls
              state={queueStatus.state}
              hasRunBefore={hasRunBefore}
              hasRunInFlight={queueStatus.runningTaskId !== null}
            />

            <h3>Up next</h3>
            <QueuePlanList plan={queueStatus.plan} />

            <h3>Completed this session</h3>
            <SessionOutcomesList outcomes={sessionOutcomes} />
          </>
        )}
      </section>

      {runEnvironment && (
        <p className="muted runs-environment-note">
          Environment: {runEnvironment === "inherit" ? "Inherit (default)" : "Strict / local"}.{" "}
          {runEnvironment === "inherit"
            ? overheadNote ??
              "Inheriting your Claude Code environment adds a fixed setup cost to every run."
            : "Only each repository's own CLAUDE.md and project settings reach a run."}{" "}
          Change this in Settings → Instructions; a finished run's own cost shows on its task's
          detail panel.
        </p>
      )}

      {runningTasks === null && !readError && <p className="muted">Reading…</p>}

      {runningTasks && runningTasks.length === 0 && (
        <EmptyState
          title="Nothing running right now"
          body="Each run lands here with its elapsed time, turn count, current tool call, recent assistant text, and a Cancel button, for as long as it is in progress."
          arrivesIn="Full history, diffs and the transcript viewer land in task 015."
        />
      )}

      {runningTasks && runningTasks.length > 0 && (
        <div className="active-runs-list">
          {runningTasks.map((task) => (
            <ActiveRunCard
              key={task.id}
              task={task}
              repositoryName={repositoryNames.get(task.repositoryId) ?? task.repositoryId}
            />
          ))}
        </div>
      )}
    </div>
  );
}
