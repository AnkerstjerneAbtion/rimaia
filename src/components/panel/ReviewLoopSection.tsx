import { useEffect, useRef, useState } from "react";

import { useReviewLevel } from "../../hooks/useReviewLevel";
import { getStrategyCatalogue, getTask, setTaskReview, toRimaiaError } from "../../lib/commands";
import type { Catalogue, ReviewConfig, RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { ReviewConfigFields } from "../ReviewConfigFields";

interface ReviewLoopSectionProps {
  readonly taskId: string;
  /** The task's own override, off `TaskDetail`. `undefined` while it loads. */
  readonly reviewInstructions: string | null | undefined;
}

/** A blank override is no override: the same rule `ExtraInstructionsEditor`
 *  states for its neighbouring column, and core's `effective_instructions`
 *  agrees (a blank one falls back to the global text). */
function normalize(value: string): string | null {
  return value.trim() === "" ? null : value;
}

/**
 * The task's own review settings (task 037, ADR-0017): an override of the
 * review instructions, and the same loop configuration as Settings → Review
 * and each repository, one level down.
 *
 * Both halves are written by one command, `set_task_review`, so each save
 * reads the other half fresh first. The task's own stored settings are what
 * `get_task` answers.
 *
 * Kept as a section that can take another row: tasks 045 and 061 add revisions
 * and consent for the instructions here.
 */
export function ReviewLoopSection({ taskId, reviewInstructions }: ReviewLoopSectionProps) {
  const level = useReviewLevel("task", taskId);
  const [catalogue, setCatalogue] = useState<Catalogue | null>(null);

  useEffect(() => {
    getStrategyCatalogue().then(
      (view) => setCatalogue(view.catalogue),
      () => {},
    );
  }, []);

  async function saveConfig(config: ReviewConfig) {
    const current = await getTask(taskId);
    await setTaskReview(taskId, current.reviewInstructions, config);
    await level.reload();
  }

  return (
    <section className="task-detail-section review-loop-section">
      <h4>Review loop</h4>
      <p className="task-detail-note muted">
        Whether a fresh agent reviews this task after it is implemented, and how hard it
        tries to fix what it finds.
      </p>
      {level.error && <ErrorBanner error={level.error} onDismiss={level.dismissError} />}

      {reviewInstructions !== undefined && (
        <ReviewInstructionsOverride
          key={taskId}
          taskId={taskId}
          initialValue={reviewInstructions ?? ""}
        />
      )}

      {level.data === null ? (
        !level.error && <p className="muted">Reading…</p>
      ) : (
        <ReviewConfigFields
          scope="task"
          idPrefix={`review-task-${taskId}`}
          level={level.data}
          catalogue={catalogue}
          onChange={saveConfig}
        />
      )}
    </section>
  );
}

/** Shaped like `ExtraInstructionsEditor`: uncontrolled, committed on blur or
 *  unmount, for the reason that editor gives (React 19 detaches refs before
 *  effect cleanups run). */
function ReviewInstructionsOverride({
  taskId,
  initialValue,
}: {
  readonly taskId: string;
  readonly initialValue: string;
}) {
  const draftRef = useRef(initialValue);
  const lastSavedRef = useRef(initialValue);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  async function save(value: string) {
    const current = await getTask(taskId);
    await setTaskReview(taskId, normalize(value), current.reviewConfig);
  }

  function commit(value: string) {
    draftRef.current = value;
    if (value === lastSavedRef.current) return;
    setSaving(true);
    setError(null);
    save(value).then(
      () => {
        lastSavedRef.current = value;
        setSaving(false);
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  useEffect(() => {
    return () => {
      if (draftRef.current !== lastSavedRef.current) {
        save(draftRef.current).catch(() => {});
      }
    };
  }, [taskId]);

  return (
    <div className="review-instructions-override">
      <label htmlFor={`review-instructions-${taskId}`}>Review instructions</label>
      <p className="task-detail-note muted">
        Replaces the global review instructions for this task. Leave empty to use them.
      </p>
      {saving && <span className="muted">Saving…</span>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <textarea
        id={`review-instructions-${taskId}`}
        className="extra-instructions-textarea"
        defaultValue={initialValue}
        onChange={(event) => {
          draftRef.current = event.target.value;
        }}
        onBlur={(event) => commit(event.target.value)}
      />
    </div>
  );
}
