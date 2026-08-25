import { useEffect, useRef, useState } from "react";

import { toRimaiaError, updateTask } from "../../lib/commands";
import type { RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface ExtraInstructionsEditorProps {
  readonly taskId: string;
  readonly initialValue: string;
}

/** A blank field is NULL, not `''` - the same rule the schema states for its
 *  neighbouring column (`initial_schema.sql`'s `plan` comment), just not
 *  enforced by a CHECK for this one. Both save paths below go through here,
 *  so a field cleared and then closed with `Esc` cannot store a second
 *  spelling of "none" that a blurred one would not. */
function normalize(value: string): string | null {
  return value.trim() === "" ? null : value;
}

/**
 * A short textarea appended after the plan when a run's prompt is composed
 * (ADR-0009) — same uncontrolled, commit-on-blur-or-unmount shape as
 * {@link "./PlanEditor" | PlanEditor}, scaled down: no preview, no save
 * button, because there is nothing here worth a second control.
 */
export function ExtraInstructionsEditor({ taskId, initialValue }: ExtraInstructionsEditorProps) {
  // The draft, mirrored out of the DOM per keystroke because React 19
  // detaches refs before effect cleanups run — see `PlanEditor`'s own comment
  // for the whole story; this is the same fix at a smaller scale.
  const draftRef = useRef(initialValue);
  const lastSavedRef = useRef(initialValue);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  function commit(value: string) {
    draftRef.current = value;
    if (value === lastSavedRef.current) return;
    setSaving(true);
    setError(null);
    // `lastSavedRef` tracks the raw string, not the normalized one, so the
    // no-op comparison above matches what the textarea itself can show.
    updateTask(taskId, { extraInstructions: normalize(value) }).then(
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
        updateTask(taskId, { extraInstructions: normalize(draftRef.current) }).catch(() => {});
      }
    };
  }, [taskId]);

  return (
    <section className="task-detail-section">
      <h4>Extra instructions</h4>
      <p className="task-detail-note muted">
        Appended after the plan when a run's prompt is composed — constraints or gotchas
        that do not belong in the plan itself.
      </p>
      {saving && <span className="muted">Saving…</span>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <textarea
        className="extra-instructions-textarea"
        defaultValue={initialValue}
        onChange={(event) => {
          draftRef.current = event.target.value;
        }}
        onBlur={(event) => commit(event.target.value)}
        aria-label="Extra instructions"
      />
    </section>
  );
}
