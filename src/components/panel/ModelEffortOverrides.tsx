import { useState } from "react";
import type { ChangeEvent } from "react";

import { toRimaiaError, updateTask } from "../../lib/commands";
import type { RimaiaError } from "../../types";
import { ErrorBanner } from "../ErrorBanner";

/**
 * The manual model/effort choices for task 005's plain dropdowns.
 *
 * ADR-0016 is explicit that this list has to come from configuration —
 * models ship faster than Rimaia releases — naming task 006's settings
 * accessor as the thing that will supply it. Task 006 has not landed, and
 * task 020 replaces this whole component with the full execution-strategy
 * control ADR-0016 describes (mode selector, planner proposals, per-
 * repository defaults). Until then, one named constant is the honest
 * placeholder: replacing it later is one edit here, not a hunt through JSX.
 */
export const MODEL_OPTIONS = [
  { value: "opus", label: "Opus" },
  { value: "sonnet", label: "Sonnet" },
  { value: "haiku", label: "Haiku" },
] as const;

export const EFFORT_OPTIONS = [
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
] as const;

/** The `<select>` value standing in for "no override" — never sent as
 *  itself, translated to `null` (Rimaia's own default) at the command
 *  boundary. */
const DEFAULT_VALUE = "";

interface ModelEffortOverridesProps {
  readonly taskId: string;
  readonly model: string | null;
  readonly effort: string | null;
}

/**
 * Both optional, both defaulting to Rimaia's own default (task 005) — kept
 * in one component, per task 005's own instruction, so task 020 has one
 * place to replace rather than two.
 *
 * The two `<select>`s hold their own local state rather than being
 * controlled straight off `model`/`effort` (`useState`'s lazy initializer,
 * reset by the same per-task remount every other field in this panel
 * relies on — see `TaskDetailPanel`'s own doc comment). A prop-controlled
 * select would visibly snap back to "Default" the instant it's changed,
 * because nothing updates `model`/`effort` until the mutation's own
 * `tasks:changed` round-trips back through the board; committing
 * optimistically and only reverting on a rejection is the same trade this
 * whole panel makes everywhere else.
 */
export function ModelEffortOverrides({ taskId, model, effort }: ModelEffortOverridesProps) {
  const [modelValue, setModelValue] = useState(model ?? DEFAULT_VALUE);
  const [effortValue, setEffortValue] = useState(effort ?? DEFAULT_VALUE);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [saving, setSaving] = useState(false);

  function handleModelChange(event: ChangeEvent<HTMLSelectElement>) {
    const value = event.target.value;
    if (value === modelValue) return;
    const previous = modelValue;
    setModelValue(value);
    setSaving(true);
    setError(null);
    updateTask(taskId, { model: value === DEFAULT_VALUE ? null : value }).then(
      () => setSaving(false),
      (thrown) => {
        setModelValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function handleEffortChange(event: ChangeEvent<HTMLSelectElement>) {
    const value = event.target.value;
    if (value === effortValue) return;
    const previous = effortValue;
    setEffortValue(value);
    setSaving(true);
    setError(null);
    updateTask(taskId, { effort: value === DEFAULT_VALUE ? null : value }).then(
      () => setSaving(false),
      (thrown) => {
        setEffortValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  return (
    <section className="task-detail-section">
      <h4>Model &amp; effort</h4>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <div className="model-effort-row">
        <label>
          Model
          <select value={modelValue} onChange={handleModelChange}>
            <option value={DEFAULT_VALUE}>Default</option>
            {MODEL_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          Effort
          <select value={effortValue} onChange={handleEffortChange}>
            <option value={DEFAULT_VALUE}>Default</option>
            {EFFORT_OPTIONS.map((option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ))}
          </select>
        </label>
        {saving && <span className="muted">Saving…</span>}
      </div>
    </section>
  );
}
