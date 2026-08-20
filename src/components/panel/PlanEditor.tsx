import { useEffect, useRef, useState } from "react";

import ReactMarkdown from "react-markdown";

import { toRimaiaError, updateTask } from "../../lib/commands";
import type { RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

interface PlanEditorProps {
  readonly taskId: string;
  readonly initialValue: string;
}

type Mode = "edit" | "preview";

/**
 * The main writing surface (task 005's own words for it). An
 * **uncontrolled** textarea — `ref` and `defaultValue`, never `value` and
 * `onChange` — so typing a 400-line plan never runs a React re-render per
 * keystroke; the DOM owns the text until something needs to read it (a
 * blur, a preview toggle, an unmount). Lifting every keystroke into state,
 * here or in a parent, is exactly the "re-renders the whole board on every
 * keypress" trap task 005's Build list warns against.
 *
 * Saved on blur, on switching to preview, and — best-effort — on unmount,
 * because `Esc` closing the panel (`Board`'s handler) and switching to a
 * different task (this component remounts by `key`) both unmount without a
 * preceding blur, and blur would otherwise be the only save trigger.
 */
export function PlanEditor({ taskId, initialValue }: PlanEditorProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  // What the user has typed, mirrored out of the DOM on every keystroke.
  //
  // Not an optimisation and not a duplicate of `textareaRef` — it is the only
  // copy of the text that still exists when the unmount save below runs.
  // React 19 detaches refs *before* it runs effect cleanups, so
  // `textareaRef.current` is `null` by then and the backstop silently saved
  // nothing: `Esc`, or clicking another card, threw the plan away with no
  // warning. Writing a ref is not a state update, so this costs no render —
  // a controlled `value`/`onChange` textarea over a 400-line plan is exactly
  // the per-keystroke re-render task 005's Notes warn against.
  const draftRef = useRef(initialValue);
  const lastSavedRef = useRef(initialValue);
  const [mode, setMode] = useState<Mode>("edit");
  const [previewSource, setPreviewSource] = useState(initialValue);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  function commit(value: string) {
    draftRef.current = value;
    if (value === lastSavedRef.current) return;
    setSaving(true);
    setError(null);
    updateTask(taskId, { plan: value }).then(
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
    // No UI survives to show this call's own error — it is a backstop for
    // the blur handler above, not a second user-visible save path. A save
    // already in flight for the same text is re-sent rather than tracked
    // separately: `update_task` writing the same plan twice is harmless,
    // where skipping the second send on the assumption the first will land
    // would lose the edit whenever it does not.
    return () => {
      if (draftRef.current !== lastSavedRef.current) {
        updateTask(taskId, { plan: draftRef.current }).catch(() => {});
      }
    };
  }, [taskId]);

  function togglePreview() {
    if (mode === "edit") {
      const value = textareaRef.current?.value ?? draftRef.current;
      setPreviewSource(value);
      // Toggling to preview is a commit point in its own right — a click on
      // this button can beat the textarea's own blur event in some browsers,
      // and preview has to show exactly what was typed, saved or not.
      commit(value);
      setMode("preview");
    } else {
      setMode("edit");
    }
  }

  return (
    <section className="task-detail-section plan-editor">
      <div className="plan-editor-header">
        <h4>Plan</h4>
        <div className="plan-editor-toolbar">
          {saving && <span className="muted">Saving…</span>}
          <button type="button" onClick={togglePreview}>
            {mode === "edit" ? "Preview" : "Edit"}
          </button>
        </div>
      </div>

      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      {mode === "edit" ? (
        <textarea
          ref={textareaRef}
          className="plan-editor-textarea"
          // `previewSource`, not `initialValue`: switching modes swaps this
          // element out of the tree entirely (the branch below renders a
          // `<div>` instead), so React genuinely remounts a fresh textarea
          // on the way back from preview, and `defaultValue` has to carry
          // the last edited text or that remount would revert it to
          // whatever was on the server when the panel opened.
          defaultValue={previewSource}
          // `defaultValue` + `onChange` is still an uncontrolled textarea —
          // React only takes control of the value when `value` is passed.
          // This handler sets a ref and nothing else, so the DOM keeps
          // owning the text and no keystroke reaches the reconciler.
          onChange={(event) => {
            draftRef.current = event.target.value;
          }}
          onBlur={(event) => commit(event.target.value)}
          aria-label="Plan"
          placeholder="Markdown. Everything an agent needs to do this unattended — an empty plan blocks the task from reaching Ready."
        />
      ) : (
        <div className="plan-preview" aria-label="Plan preview">
          <ReactMarkdown>{previewSource}</ReactMarkdown>
        </div>
      )}
    </section>
  );
}
