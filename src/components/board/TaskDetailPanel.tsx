import { useEffect, useRef, useState } from "react";

import { getTask, toRimaiaError } from "../../lib/commands";
import { subscribeToTasksChanged } from "../../lib/events";
import type { Repository, RimaiaError, Task, TaskDetail } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { DeleteTaskSection } from "../panel/DeleteTaskSection";
import { DependenciesEditor } from "../panel/DependenciesEditor";
import { ExtraInstructionsEditor } from "../panel/ExtraInstructionsEditor";
import { LinksEditor } from "../panel/LinksEditor";
import { PlanEditor } from "../panel/PlanEditor";
import { RepositorySelector } from "../panel/RepositorySelector";
import { RetrySection } from "../panel/RetrySection";
import { RunHistorySection } from "../panel/RunHistorySection";
import { RunInfoSection } from "../panel/RunInfoSection";
import { RunOutcomeSection } from "../panel/RunOutcomeSection";
import { StrategySection } from "../panel/StrategySection";
import { TitleField } from "../panel/TitleField";
import { WorktreeSection } from "../panel/WorktreeSection";
import { COLUMN_TITLES } from "./Column";
import { RunStateBadge } from "./RunStateBadge";

interface TaskDetailPanelProps {
  readonly task: Task;
  readonly repositoryName: string;
  /**
   * What the repository selector may offer (seam-contract D13). Optional
   * because the panel can always name the repository the task is already
   * filed under — `repositoryName` is that — and a caller with no list is
   * not broken, only limited to showing it. `Board` has the list already and
   * passes it.
   */
  readonly repositories?: readonly Repository[];
  readonly onClose: () => void;
}

/**
 * Task 005's task detail panel — the main writing surface. A thin composer
 * over small sections, the same shape `SettingsView` uses over its own
 * sections: each section below owns its own request/error state and calls
 * `lib/commands.ts` directly, so tasks 007, 008 and 009 (which the stage 3
 * brief names as all editing this file next, for branch/worktree/status,
 * last-run outcome and queue position respectively) add a section and one
 * line of composition here, rather than touching another section's code.
 * Task 007's `WorktreeSection` is the first of the three to land.
 *
 * Wrapped so switching the selected task (or `Esc` closing and reopening a
 * different one) remounts the body by `key`. That is what resets every
 * uncontrolled field below to the new task's own values instead of either
 * carrying a stale draft across or clobbering an in-progress edit with a
 * prop update — `Board.tsx` renders one `<TaskDetailPanel>` without a `key`
 * of its own, so the reset has to happen in here.
 *
 * It is an **overlay drawer**, not a flex sibling of the columns: it is
 * painted over the board and takes no layout width from it (see
 * `board.css`'s arithmetic, which is the only record of it — jsdom has no
 * layout engine, so no test can check a width). Deliberately *not* a modal:
 * no scrim, no focus trap, no `aria-modal`, no `<dialog>.showModal()`. The
 * board behind it stays live — clicking another card switches this panel to
 * it, and a drag still works in the columns the drawer does not cover —
 * `Esc` closes it through `Board`'s own single handler (a modal `<dialog>`
 * would add a second, native close path that handler could not coordinate
 * with an in-progress keyboard drag), and Tab simply leaves at the end of
 * the panel because it is last in the board's DOM order. The only piece a
 * non-modal overlay still owes a keyboard user is focus itself: it moves in
 * here on open, and `Board` returns it to the card that opened it on close.
 */
export function TaskDetailPanel(props: TaskDetailPanelProps) {
  return <TaskDetailPanelBody key={props.task.id} {...props} />;
}

function TaskDetailPanelBody({
  task,
  repositoryName,
  repositories = [],
  onClose,
}: TaskDetailPanelProps) {
  const [detail, setDetail] = useState<TaskDetail | null>(null);
  const [detailError, setDetailError] = useState<RimaiaError | null>(null);
  const panelRef = useRef<HTMLElement>(null);

  // Focus follows the drawer in. Without this the panel opens *over* the
  // board while focus stays on the card behind it, so a keyboard user's next
  // Tab walks the board they can no longer see. The panel itself takes the
  // focus rather than its first field: it carries the label a screen reader
  // reads out ("Task: …"), and landing in the title input would put a
  // keystroke's worth of typing into a rename nobody asked for.
  useEffect(() => {
    panelRef.current?.focus();
  }, []);

  // Fetches `TaskDetail` (links, dependsOn, lastRun — the fields `Task`
  // itself doesn't carry, per the board agent's own BLOCKED note: only
  // `get_task` has them) once on mount, and again on any `tasks:changed`
  // that could plausibly mean this task, since the mutations `LinksEditor`
  // performs are the only other thing that keeps it fresh otherwise.
  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    function load() {
      getTask(task.id).then(
        (result) => {
          if (active) {
            setDetail(result);
            setDetailError(null);
          }
        },
        (thrown) => {
          if (active) setDetailError(toRimaiaError(thrown));
        },
      );
    }

    load();

    // Unlike the board's own `tasks:changed` handling (which always
    // re-reads the whole list, because it shows one), this panel cares
    // about exactly one task, so it narrows: an empty array is ADR-0018's
    // "re-read wholesale" recovery case and always triggers a refetch; a
    // non-empty array only does when this task's id is in it.
    subscribeToTasksChanged((taskIds) => {
      if (active && (taskIds.length === 0 || taskIds.includes(task.id))) load();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the mutations
        // this panel itself performs still refresh via their own `onChanged`.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, [task.id]);

  function refreshDetail() {
    getTask(task.id).then(
      (result) => {
        setDetail(result);
        setDetailError(null);
      },
      (thrown) => setDetailError(toRimaiaError(thrown)),
    );
  }

  const detailLoading = detail === null && detailError === null;

  return (
    <aside
      ref={panelRef}
      className="task-detail-panel"
      aria-label={`Task: ${task.title}`}
      tabIndex={-1}
    >
      <div className="task-detail-header">
        <TitleField taskId={task.id} initialTitle={task.title} />
        <button type="button" className="task-detail-close" onClick={onClose} aria-label="Close">
          Esc
        </button>
      </div>

      <dl className="detail-list">
        <dt>Repository</dt>
        <dd>
          <RepositorySelector
            taskId={task.id}
            repositoryId={task.repositoryId}
            repositories={repositories}
            repositoryName={repositoryName}
            worktreePath={task.worktreePath}
            hasRuns={detail?.lastRun != null}
            detailLoading={detailLoading}
          />
        </dd>
        <dt>Column</dt>
        <dd>{COLUMN_TITLES[task.column]}</dd>
        <dt>Run state</dt>
        <dd>
          <RunStateBadge runState={task.runState} lastRun={detail?.lastRun ?? null} />
          {task.runState === "idle" && <span className="muted">Idle</span>}
        </dd>
      </dl>

      {detailError && (
        <ErrorBanner error={detailError} onDismiss={() => setDetailError(null)} />
      )}

      <PlanEditor taskId={task.id} initialValue={task.plan ?? ""} />

      <ExtraInstructionsEditor taskId={task.id} initialValue={task.extraInstructions ?? ""} />

      <LinksEditor
        taskId={task.id}
        links={detail?.links ?? []}
        loading={detailLoading}
        onChanged={refreshDetail}
      />

      {/* ADR-0008's dependency editor. `dependsOn` is on the detail rather than
          on `Task`, so it waits for the same fetch `LinksEditor` does; the
          repository comes off the board's own row, because a dependency must be
          in the same one and that is also what the picker filters by. */}
      <DependenciesEditor
        taskId={task.id}
        repositoryId={task.repositoryId}
        dependsOn={detail?.dependsOn ?? []}
        loading={detailLoading}
        onChanged={refreshDetail}
      />

      {/* Task 020's execution-strategy control, where task 005's plain
          model/effort dropdowns used to sit. The mode, the two choices and
          the plan envelope come off the board's own `Task` (they are columns,
          and the board re-reads them on `tasks:changed`); the effective
          triple comes off `detail`, because it is resolved per read and is
          not on a `Task` at all (seam-contract D12's amendment). */}
      <StrategySection
        taskId={task.id}
        strategyMode={task.strategyMode}
        model={task.model}
        effort={task.effort}
        strategyPlan={task.strategyPlan}
        strategySource={task.strategySource}
        effective={detail}
        loading={detailLoading}
        onChanged={refreshDetail}
      />

      <RunInfoSection
        branch={task.branch}
        worktreePath={task.worktreePath}
        lastRun={detail?.lastRun ?? null}
        loading={detailLoading}
      />

      {/* Task 008's own section: the last run's outcome (exit class, cost,
          error text, PR link) and its log path — additive to `RunInfoSection`
          above, not a replacement for it (see this stage's file-ownership
          note in `RunOutcomeSection.tsx`'s own doc comment). */}
      <RunOutcomeSection lastRun={detail?.lastRun ?? null} loading={detailLoading} />

      {/* Task 014's manual pair, immediately under the outcome that caused the
          wait — "give up" is only a decision if the error is on screen with it.
          Renders nothing at all unless this task is `waiting_retry`. */}
      <RetrySection
        taskId={task.id}
        runState={task.runState}
        lastRun={detail?.lastRun ?? null}
        loading={detailLoading}
        onChanged={refreshDetail}
      />

      {/* Task 015's full history — every attempt, not only the last one —
          each opening the run detail overlay (outcome, diff, commits, PR,
          prompt, transcript, in ADR-0013's order). */}
      <RunHistorySection taskId={task.id} />

      <WorktreeSection taskId={task.id} />

      <DeleteTaskSection taskId={task.id} title={task.title} onDeleted={onClose} />
    </aside>
  );
}
