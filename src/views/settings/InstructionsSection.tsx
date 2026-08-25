import { useEffect, useRef, useState } from "react";
import type { ChangeEvent } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getBaseInstructions,
  getRunEnvironment,
  listTasks,
  previewComposedPrompt,
  setBaseInstructions,
  setRunEnvironment,
  toRimaiaError,
} from "../../lib/commands";
import type { RimaiaError, RunEnvironment, TaskSummary } from "../../types";

const RUN_ENVIRONMENT_OPTIONS: ReadonlyArray<{
  readonly value: RunEnvironment;
  readonly label: string;
  readonly description: string;
}> = [
  {
    value: "inherit",
    label: "Inherit (default)",
    description:
      "The full Claude Code environment you use interactively — your MCP servers, hooks, " +
      "plugins and output styles. Measured against the spike's own one-word prompt, this " +
      "costs roughly 3.6× per run before any work happens.",
  },
  {
    value: "strict_local",
    label: "Strict / local",
    description:
      "Only the repository's own CLAUDE.md and project settings " +
      "(--strict-mcp-config --setting-sources project,local). No personal MCP servers, " +
      "hooks or plugins reach the run.",
  },
];

/**
 * Settings → Instructions (task 006, ADR-0009, ADR-0012). Three concerns
 * that share one panel because they share one purpose — everything a run
 * receives besides its own plan:
 *
 * 1. `settings.base_instructions`, edited here.
 * 2. `settings.run_environment` — ADR-0004's amendment names this UI's own
 *    obligation: state the ~3.6× cost of `inherit` plainly, not soften it,
 *    and keep the toggle within reach of the per-run cost task 008 adds.
 * 3. A live preview of a chosen task's fully composed prompt, fetched from
 *    the backend's own `compose_prompt` (`preview_composed_prompt`) so it is
 *    never a second, frontend-side implementation of composition — ADR-0009
 *    is explicit that composition lives in exactly one place.
 */
export function InstructionsSection() {
  const [baseInstructions, setBaseInstructionsState] = useState<string | null>(null);
  const [instructionsError, setInstructionsError] = useState<RimaiaError | null>(null);
  // Bumped whenever a base-instructions save lands, so the preview below
  // (if a task is already selected) re-composes against what a run would
  // now actually receive rather than what it showed a moment ago.
  const [previewNonce, setPreviewNonce] = useState(0);

  useEffect(() => {
    getBaseInstructions().then(setBaseInstructionsState, (thrown) =>
      setInstructionsError(toRimaiaError(thrown)),
    );
  }, []);

  return (
    <section className="panel instructions-section">
      <h3>Instructions</h3>

      <div className="instructions-subsection">
        <h4>Base instructions</h4>
        <p className="muted">
          Applied to every run, ahead of the task&rsquo;s own plan (ADR-0009). Supports{" "}
          <code>{"{{task.title}}"}</code>, <code>{"{{task.branch}}"}</code>,{" "}
          <code>{"{{repo.name}}"}</code>, <code>{"{{repo.default_branch}}"}</code> and{" "}
          <code>{"{{task.links}}"}</code> — an unknown variable passes through untouched
          rather than failing the run.
        </p>
        {instructionsError && (
          <ErrorBanner error={instructionsError} onDismiss={() => setInstructionsError(null)} />
        )}
        {baseInstructions === null ? (
          !instructionsError && <p className="muted">Reading…</p>
        ) : (
          <BaseInstructionsEditor
            initialValue={baseInstructions}
            onSaved={() => setPreviewNonce((n) => n + 1)}
          />
        )}
      </div>

      <div className="instructions-subsection">
        <h4>Run environment</h4>
        <RunEnvironmentToggle />
      </div>

      <div className="instructions-subsection">
        <h4>Preview</h4>
        <p className="muted">
          The exact prompt a run would receive right now, for a chosen task — byte for byte
          what <code>compose_prompt</code> produces for a real run, not an approximation.
        </p>
        <ComposedPromptPreview refreshSignal={previewNonce} />
      </div>
    </section>
  );
}

interface BaseInstructionsEditorProps {
  readonly initialValue: string;
  /** Fired after every successful save — including the unmount backstop —
   *  so the live preview above can catch up. */
  readonly onSaved: () => void;
}

/**
 * Same uncontrolled, commit-on-blur-or-unmount shape as `PlanEditor` and
 * `ExtraInstructionsEditor`: a ref carries the draft because React 19
 * detaches `ref`s before running effect cleanups, so reading the DOM back at
 * unmount would read `null` — see `PlanEditor`'s own comment for the full
 * story of the bug this avoids reintroducing. No preview-toggle apparatus
 * here (unlike `PlanEditor`'s Markdown-vs-plain toggle): this text is never
 * rendered as Markdown *by this editor* — the composed-prompt preview below
 * is the only place base instructions are shown back, and it shows them
 * exactly as sent (task 006's own instruction), not re-rendered.
 */
function BaseInstructionsEditor({ initialValue, onSaved }: BaseInstructionsEditorProps) {
  const draftRef = useRef(initialValue);
  const lastSavedRef = useRef(initialValue);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  function commit(value: string) {
    draftRef.current = value;
    if (value === lastSavedRef.current) return;
    setSaving(true);
    setError(null);
    setBaseInstructions(value).then(
      () => {
        lastSavedRef.current = value;
        setSaving(false);
        onSaved();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  useEffect(() => {
    // Backstop for leaving Settings (or closing the app) without a preceding
    // blur — the unmount-flush `PlanEditor` needs, for the same reason.
    // `onSaved` is intentionally not a dependency: it would only ever be a
    // fresh closure over the same `setPreviewNonce` updater, and adding it
    // would re-run this effect (tearing down and rebuilding the cleanup) on
    // every parent render for no behavioural difference.
    return () => {
      if (draftRef.current !== lastSavedRef.current) {
        setBaseInstructions(draftRef.current).then(onSaved, () => {});
      }
    };
  }, []);

  return (
    <>
      {saving && <span className="muted">Saving…</span>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <textarea
        className="instructions-editor-textarea"
        defaultValue={initialValue}
        onChange={(event) => {
          draftRef.current = event.target.value;
        }}
        onBlur={(event) => commit(event.target.value)}
        aria-label="Base instructions"
        placeholder="Markdown. Applied to every run, before the task's own plan."
      />
    </>
  );
}

/**
 * ADR-0004's amendment two-modes toggle. Committed optimistically and
 * reverted on rejection — the same trade `ModelEffortOverrides` makes for a
 * task-scoped setting, here for a global one.
 */
function RunEnvironmentToggle() {
  const [value, setValue] = useState<RunEnvironment | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getRunEnvironment().then(setValue, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  function handleChange(next: RunEnvironment) {
    if (next === value) return;
    const previous = value;
    setValue(next);
    setSaving(true);
    setError(null);
    setRunEnvironment(next).then(
      () => setSaving(false),
      (thrown) => {
        setValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  return (
    <div className="run-environment">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {value === null ? (
        !error && <p className="muted">Reading…</p>
      ) : (
        <div role="radiogroup" aria-label="Run environment" className="run-environment-options">
          {RUN_ENVIRONMENT_OPTIONS.map((option) => (
            <label key={option.value} className="run-environment-option">
              <input
                type="radio"
                name="run-environment"
                value={option.value}
                aria-label={option.label}
                checked={value === option.value}
                disabled={saving}
                onChange={() => handleChange(option.value)}
              />
              <span className="run-environment-option-body">
                <strong>{option.label}</strong>
                <span className="muted">{option.description}</span>
              </span>
            </label>
          ))}
        </div>
      )}
      {saving && <span className="muted">Saving…</span>}
      <p className="muted run-environment-cost-note">
        Per-run cost (<code>result.total_cost_usd</code>) shows beside this toggle once task
        008 lands.
      </p>
    </div>
  );
}

interface ComposedPromptPreviewProps {
  /** Bumped by the base-instructions editor after a save — re-fetches the
   *  preview for whatever task is currently selected, if any. */
  readonly refreshSignal: number;
}

/**
 * The byte-for-byte preview task 006's acceptance criterion names. Always
 * reads through `previewComposedPrompt`, never composes anything itself —
 * see this file's own header comment on why that is not optional.
 *
 * Rendered as `<pre>`, not through `react-markdown`: the point is to show
 * exactly what the agent receives, whitespace included, not a rendering of
 * it.
 */
function ComposedPromptPreview({ refreshSignal }: ComposedPromptPreviewProps) {
  const [tasks, setTasks] = useState<TaskSummary[] | null>(null);
  const [tasksError, setTasksError] = useState<RimaiaError | null>(null);
  const [selectedTaskId, setSelectedTaskId] = useState("");
  const [previewText, setPreviewText] = useState<string | null>(null);
  const [previewLoading, setPreviewLoading] = useState(false);
  const [previewError, setPreviewError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    listTasks().then(setTasks, (thrown) => setTasksError(toRimaiaError(thrown)));
  }, []);

  useEffect(() => {
    if (!selectedTaskId) return;
    // Guards against a response for a task the user has since navigated away
    // from landing after a later one — picking task A then quickly B must not
    // let A's slower response overwrite B's preview (or clear its loading
    // state) if A resolves last. Same shape as `WorktreeSection`'s own fetch.
    let active = true;
    setPreviewLoading(true);
    setPreviewError(null);
    previewComposedPrompt(selectedTaskId).then(
      (text) => {
        if (active) {
          setPreviewText(text);
          setPreviewLoading(false);
        }
      },
      (thrown) => {
        if (active) {
          setPreviewError(toRimaiaError(thrown));
          setPreviewLoading(false);
        }
      },
    );
    // `refreshSignal` deliberately participates here even though it is never
    // read: it exists only to make a base-instructions save re-run this
    // effect for whichever task is already selected.
    return () => {
      active = false;
    };
  }, [selectedTaskId, refreshSignal]);

  function handleTaskChange(event: ChangeEvent<HTMLSelectElement>) {
    const taskId = event.target.value;
    setSelectedTaskId(taskId);
    setPreviewText(null);
    setPreviewError(null);
  }

  return (
    <div className="instructions-preview">
      <label className="instructions-preview-picker">
        Preview task
        <select aria-label="Preview task" value={selectedTaskId} onChange={handleTaskChange}>
          <option value="">Choose a task…</option>
          {(tasks ?? []).map((task) => (
            <option key={task.id} value={task.id}>
              {task.title}
            </option>
          ))}
        </select>
      </label>
      {tasksError && <ErrorBanner error={tasksError} onDismiss={() => setTasksError(null)} />}
      {previewError && (
        <ErrorBanner error={previewError} onDismiss={() => setPreviewError(null)} />
      )}
      {!selectedTaskId && !tasksError && (
        <p className="muted">Choose a task to see exactly what its next run would receive.</p>
      )}
      {previewLoading && <span className="muted">Composing…</span>}
      {selectedTaskId && !previewLoading && previewText !== null && (
        <pre className="instructions-preview-output" aria-label="Composed prompt preview">
          {previewText}
        </pre>
      )}
    </div>
  );
}
