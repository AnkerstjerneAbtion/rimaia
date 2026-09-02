import { useCallback, useEffect, useState } from "react";
import type { FormEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getRunCapacity,
  setMaxConcurrency,
  setScheduleMode,
  toRimaiaError,
} from "../../lib/commands";
import { subscribeToSettingsChanged } from "../../lib/events";
import type { RimaiaError, RunCapacity, ScheduleMode } from "../../types";

const MODE_OPTIONS: ReadonlyArray<{
  readonly value: ScheduleMode;
  readonly label: string;
  readonly description: string;
}> = [
  {
    value: "sequential",
    label: "One at a time",
    description:
      "The safe default, and what “implement these in this order” means. The next task starts when the previous one reaches a terminal state.",
  },
  {
    value: "parallel",
    label: "Several at once",
    description:
      "An evening of independent tasks across several repositories finishes far sooner. Each repository still holds one run at a time unless you raise its own limit.",
  },
];

/**
 * Settings → Concurrency (task 012, ADR-0010).
 *
 * Two controls with one subtlety between them, which is why they are one panel
 * rather than two: **the limit is shown as stored, not as resolved.** Sequential
 * mode runs exactly one task whatever the number says, and the number is
 * deliberately left alone when the mode is switched, so flipping back to
 * "several at once" restores the value that was chosen rather than a default. A
 * panel that rendered the resolved `1` would make the setting look forgotten
 * every time the mode changed — so the number stays visible and the mode's own
 * description is what says it is not in force.
 *
 * The ceiling is read off the backend rather than written here. It is
 * `CONCURRENCY_CEILING`, a Rust constant whose whole purpose is that there is
 * exactly one of it, and a hard-coded `8` in this file would be the second.
 *
 * Two commit strategies, on purpose. The mode is a radio and commits
 * optimistically, reverting on rejection — the trade `RunEnvironmentToggle`
 * already makes, and a mode has no invalid value to send. The limit is a form
 * with a Save button, because a number input fires `change` on every keystroke
 * and "4" typed on the way to "4" would otherwise be a write; the same argument
 * `McpSection` makes for its port field.
 */
export function ConcurrencySection() {
  const [capacity, setCapacity] = useState<RunCapacity | null>(null);
  const [readError, setReadError] = useState<RimaiaError | null>(null);
  const [limit, setLimit] = useState("");
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<RimaiaError | null>(null);

  const read = useCallback(async () => {
    try {
      const current = await getRunCapacity();
      setCapacity(current);
      setLimit(String(current.maxConcurrency));
      setReadError(null);
    } catch (thrown) {
      setReadError(toRimaiaError(thrown));
    }
  }, []);

  useEffect(() => {
    void read();
  }, [read]);

  useEffect(() => {
    // Both keys live in `settings`, so any other writer — the MCP tool, or a
    // second window — announces itself on this channel.
    const subscription = subscribeToSettingsChanged(() => {
      void read();
    });
    return () => {
      void subscription.then((unlisten) => unlisten());
    };
  }, [read]);

  function handleModeChange(next: ScheduleMode) {
    if (capacity === null || next === capacity.mode || saving) return;
    const previous = capacity;
    setCapacity({ ...capacity, mode: next });
    setSaving(true);
    setSaveError(null);
    setScheduleMode(next).then(
      (updated) => {
        setCapacity(updated);
        setLimit(String(updated.maxConcurrency));
        setSaving(false);
      },
      (thrown) => {
        setCapacity(previous);
        setSaveError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  const parsedLimit = Number(limit);
  const limitIsLegal =
    capacity !== null &&
    Number.isInteger(parsedLimit) &&
    parsedLimit >= 1 &&
    parsedLimit <= capacity.ceiling;
  const limitChanged = capacity !== null && parsedLimit !== capacity.maxConcurrency;

  async function handleLimitSave(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!limitIsLegal || !limitChanged || saving) return;

    setSaving(true);
    setSaveError(null);
    try {
      const updated = await setMaxConcurrency(parsedLimit);
      setCapacity(updated);
      setLimit(String(updated.maxConcurrency));
    } catch (thrown) {
      setSaveError(toRimaiaError(thrown));
    } finally {
      setSaving(false);
    }
  }

  return (
    <section className="panel concurrency-section">
      <h3>Concurrency</h3>
      {readError && <ErrorBanner error={readError} onDismiss={() => setReadError(null)} />}
      {capacity === null && !readError && <p className="muted">Reading…</p>}

      {capacity !== null && (
        <>
          <div role="radiogroup" aria-label="Run mode" className="schedule-mode-options">
            {MODE_OPTIONS.map((option) => (
              <label key={option.value} className="schedule-mode-option">
                <input
                  type="radio"
                  name="schedule-mode"
                  value={option.value}
                  aria-label={option.label}
                  checked={capacity.mode === option.value}
                  disabled={saving}
                  onChange={() => handleModeChange(option.value)}
                />
                <span className="schedule-mode-option-body">
                  <strong>{option.label}</strong>
                  <span className="muted">{option.description}</span>
                </span>
              </label>
            ))}
          </div>

          <form className="concurrency-limit-form" onSubmit={handleLimitSave}>
            <label htmlFor="max-concurrency">Runs at once</label>
            <input
              id="max-concurrency"
              type="number"
              min={1}
              max={capacity.ceiling}
              value={limit}
              disabled={saving}
              onChange={(event) => setLimit(event.target.value)}
            />
            <button type="submit" disabled={saving || !limitIsLegal || !limitChanged}>
              Save
            </button>
            {saving && <span className="muted">Saving…</span>}
          </form>
          <p className="muted">
            At most {capacity.ceiling}, whatever is typed here — a mis-set value must not be
            able to spawn ten agents.
            {capacity.mode === "sequential" &&
              " Not in force while runs happen one at a time; it is kept so switching back restores it."}
          </p>
        </>
      )}

      {saveError && <ErrorBanner error={saveError} onDismiss={() => setSaveError(null)} />}
    </section>
  );
}
