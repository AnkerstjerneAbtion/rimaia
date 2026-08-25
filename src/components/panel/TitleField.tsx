import { useEffect, useRef, useState } from "react";

import { toRimaiaError, updateTask } from "../../lib/commands";
import type { RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface TitleFieldProps {
  readonly taskId: string;
  readonly initialTitle: string;
}

/**
 * The panel's header field. Same uncontrolled, commit-on-blur-or-unmount
 * shape as {@link "./PlanEditor" | PlanEditor}, including the draft ref that
 * makes the unmount save actually fire; `title` is `NOT NULL`, so a blank
 * commit is dropped rather than sent.
 *
 * It lives in `panel/` rather than inside `TaskDetailPanel.tsx` because that
 * is where its own test can reach it — the three fields that shared the dead
 * unmount backstop are only provably fixed one at a time, and the panel's
 * composition test proves composition, not this.
 */
export function TitleField({ taskId, initialTitle }: TitleFieldProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  // Survives the unmount that `inputRef` does not — React 19 detaches refs
  // before effect cleanups run, so the backstop below used to read `null` and
  // silently drop a renamed title. See `PlanEditor`'s comment for the whole
  // story.
  const draftRef = useRef(initialTitle);
  const lastSavedRef = useRef(initialTitle);
  const [error, setError] = useState<RimaiaError | null>(null);

  function commit(value: string) {
    const trimmed = value.trim();
    if (trimmed === "") {
      // `title` is `NOT NULL` - core's own rule is "a task's title must not
      // be blank" (`tasks/service.rs::validate_title`). Surfacing that
      // refusal here (rather than sending it and letting the backend say
      // no) avoids a round trip for a rule the frontend already knows, and
      // restoring the last saved value keeps the field from being left
      // visibly blank with no save and no explanation. Assigning `.value`
      // fires no `change` event, so the draft is restored by hand or the
      // unmount save below would still be holding the blank.
      setError({ code: "invalid", message: "a task's title must not be blank" });
      draftRef.current = lastSavedRef.current;
      if (inputRef.current) inputRef.current.value = lastSavedRef.current;
      return;
    }
    draftRef.current = trimmed;
    if (trimmed === lastSavedRef.current) return;
    setError(null);
    updateTask(taskId, { title: trimmed }).then(
      () => {
        lastSavedRef.current = trimmed;
      },
      (thrown) => setError(toRimaiaError(thrown)),
    );
  }

  useEffect(() => {
    // A backstop for the blur handler, not a second user-visible save path:
    // nothing survives to render this call's own error.
    return () => {
      const value = draftRef.current.trim();
      if (value !== "" && value !== lastSavedRef.current) {
        updateTask(taskId, { title: value }).catch(() => {});
      }
    };
  }, [taskId]);

  return (
    <div className="task-detail-title">
      <input
        ref={inputRef}
        type="text"
        className="task-detail-title-input"
        defaultValue={initialTitle}
        aria-label="Task title"
        onChange={(event) => {
          draftRef.current = event.target.value;
        }}
        onBlur={(event) => commit(event.target.value)}
      />
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
    </div>
  );
}
