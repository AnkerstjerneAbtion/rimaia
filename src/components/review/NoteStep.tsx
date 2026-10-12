import type { KeyboardEvent } from "react";

import type { RimaiaError, TaskDependent } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

export type NoteKind = "reject" | "request_changes";

const COPY: Record<NoteKind, { heading: string; consequence: string; submit: string }> = {
  reject: {
    heading: "Reject",
    // The whole difference between `r` and `c`, said before submission.
    consequence:
      "The task goes back to ready and its worktree is removed. Its work is kept on a set-aside branch, and the next run starts on a fresh branch from the base.",
    submit: "Reject",
  },
  request_changes: {
    heading: "Needs changes",
    consequence:
      "The task goes back to ready and keeps its worktree and branch, so the next run continues on the reviewed commits with this note added to its instructions.",
    submit: "Send back",
  },
};

interface NoteStepProps {
  readonly kind: NoteKind;
  readonly text: string;
  readonly onChange: (text: string) => void;
  readonly onSubmit: () => void;
  readonly onCancel: () => void;
  /** Dependents this verdict would block: the unarchived ones only. */
  readonly affected: readonly TaskDependent[];
  readonly pending: boolean;
  /** A service refusal, rendered as its message (seam-contract D8). The note
   *  text is kept, so the reviewer can fix the note and send again. */
  readonly error: RimaiaError | null;
}

/**
 * The note field for a reject or a needs-changes verdict. The service refuses
 * a blank note (task 034) and the refusal is shown as it came; this step adds
 * no validation of its own.
 *
 * Enter is a newline here, as in any note. Ctrl or Cmd with Enter sends it, and
 * Escape leaves without acting — and after reading the dependents warning,
 * that keystroke is the confirmation, so the review needs no second dialog.
 */
export function NoteStep({
  kind,
  text,
  onChange,
  onSubmit,
  onCancel,
  affected,
  pending,
  error,
}: NoteStepProps) {
  const copy = COPY[kind];

  function handleKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      onCancel();
      return;
    }
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      if (!pending) onSubmit();
    }
  }

  return (
    <form
      className="review-note-step"
      aria-label={`${copy.heading}: add a note`}
      onSubmit={(event) => {
        event.preventDefault();
        if (!pending) onSubmit();
      }}
    >
      <h4>{copy.heading}</h4>
      <p className="review-note-consequence">{copy.consequence}</p>
      {affected.length > 0 && (
        <div className="review-note-warning" role="note" aria-label="Downstream tasks affected">
          <p>
            Sending this back moves it out of review, so{" "}
            {affected.length === 1 ? "this task that depends" : "these tasks that depend"} on it
            will be blocked:
          </p>
          <ul>
            {affected.map((dependent) => (
              <li key={dependent.id}>{dependent.title}</li>
            ))}
          </ul>
        </div>
      )}
      <label className="review-note-label">
        <span>Note for the next run</span>
        <textarea
          autoFocus
          rows={4}
          value={text}
          onChange={(event) => onChange(event.target.value)}
          onKeyDown={handleKeyDown}
        />
      </label>
      {error && <ErrorBanner error={error} />}
      <div className="review-note-actions">
        <button type="submit" className="btn-primary" disabled={pending}>
          {copy.submit}
        </button>
        <button type="button" onClick={onCancel} disabled={pending}>
          Cancel
        </button>
      </div>
    </form>
  );
}
