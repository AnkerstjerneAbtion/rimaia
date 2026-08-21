import { useEffect, useState } from "react";

import { ActiveRunCard } from "../components/runs/ActiveRunCard";
import { EmptyState } from "../components/EmptyState";
import { ErrorBanner } from "../components/ErrorBanner";
import { getRunEnvironment, listRepositories, listTasks, toRimaiaError } from "../lib/commands";
import { subscribeToTasksChanged } from "../lib/events";
import type { RimaiaError, RunEnvironment, TaskSummary } from "../types";

/**
 * Task 008's Runs view: whatever task is running right now, live, with a
 * Cancel button — the "riskiest component" ADR-0014 names, made visible.
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

  return (
    <div className="runs-view">
      {readError && <ErrorBanner error={readError} onDismiss={() => setReadError(null)} />}

      {runEnvironment && (
        <p className="muted runs-environment-note">
          Environment: {runEnvironment === "inherit" ? "Inherit (default)" : "Strict / local"}.{" "}
          {runEnvironment === "inherit"
            ? "Inheriting your Claude Code environment costs roughly 3.6× per run over strict/local."
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
