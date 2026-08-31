import { useCallback, useEffect, useRef, useState } from "react";

import { ErrorBanner } from "../../components/ErrorBanner";
import {
  getStrategyApproval,
  getStrategyCatalogue,
  getStrategyDefaults,
  setStrategyApproval,
  setStrategyCatalogue,
  setStrategyDefaults,
  toRimaiaError,
} from "../../lib/commands";
import { subscribeToSettingsChanged } from "../../lib/events";
import type {
  Catalogue,
  PlannerBudget,
  RimaiaError,
  StrategyApproval,
  StrategyCatalogueView,
  StrategyDefaults,
} from "../../types";
import { CatalogueSelect, StrategyDefaultsFields } from "./StrategyDefaultsFields";

/**
 * Both spellings are the stored ones, and the labels are the sentences task
 * 020 fixed rather than a paraphrase: this control decides whether an
 * overnight queue keeps moving, and "recommended for overnight queues" is the
 * whole reason `automatic` is the default an absent key reads as.
 */
const APPROVAL_OPTIONS: ReadonlyArray<{
  readonly value: StrategyApproval;
  readonly label: string;
}> = [
  {
    value: "automatic",
    label: "Run the implementation immediately after planning (recommended for overnight queues)",
  },
  { value: "manual", label: "Wait for me to accept the strategy" },
];

/**
 * Settings → Execution strategy (task 020, ADR-0016, seam-contract D17).
 *
 * Four controls over two settings keys — `strategy_default`,
 * `strategy_catalogue` and `strategy_approval` — which is why they share a
 * panel: the default a task falls through to, the lists that default is chosen
 * from, and the planner's own budget are one decision seen from three angles.
 * Per-repository defaults are the same struct one level down and live on the
 * repository row, in `RepositoriesSection`, where the repository they belong to
 * already is.
 *
 * **The catalogue is edited as raw JSON, on purpose.** It is honest rather than
 * elegant: it is configuration, it is already a JSON document in a settings
 * row a user is entitled to edit with `sqlite3` (ADR-0003), and a structured
 * editor — add/remove/reorder rows for two lists and a planner budget — is a
 * lot of surface for something edited twice a year, the day a new model ships.
 * The textarea is paired with the backend's *own* parse message rendered
 * inline, so a refused edit says exactly what serde said rather than "invalid".
 *
 * **The approval radios are stored and rendered, and nothing reads them yet.**
 * The gate itself — a planned task waiting on a human before its
 * implementation run — is deliberately deferred to a later PR so that it does
 * not contend with tasks 011 and 012's `selection` restructure (plan decision
 * 3). This is not an unwired control someone forgot: the value round-trips to
 * `settings.strategy_approval` today because a radio group that forgets its
 * answer on relaunch is worse than no radio group, and the run path will start
 * reading it without this file changing.
 */
export function StrategySection() {
  const [view, setView] = useState<StrategyCatalogueView | null>(null);
  const [viewError, setViewError] = useState<RimaiaError | null>(null);

  const read = useCallback(() => {
    getStrategyCatalogue().then(
      (next) => {
        setView(next);
        setViewError(null);
      },
      (thrown) => setViewError(toRimaiaError(thrown)),
    );
  }, []);

  useEffect(() => {
    read();
  }, [read]);

  useEffect(() => {
    // The catalogue is a settings key, so a second window adding a model
    // announces itself here — the same third-way-to-stay-fresh `McpSection`
    // relies on for `mcp_port`. Our own writes echo back through it too, which
    // is harmless: they answer with the stored view already, so the re-read
    // finds what is on screen.
    let active = true;
    let unlisten: (() => void) | undefined;
    subscribeToSettingsChanged(() => {
      if (active) read();
    }).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview): live refresh is
        // unavailable, but every write below answers with its own result.
      },
    );
    return () => {
      active = false;
      unlisten?.();
    };
  }, [read]);

  /**
   * The one writer of `strategy_catalogue` in this panel, shared by the
   * textarea and the planner dropdowns because they edit the same key. It
   * resolves with the stored view so every control repaints from what was
   * actually written — including the trimming `set_catalogue` does — and
   * rejects with the parser's own message for the caller to render where the
   * edit was made.
   */
  const storeCatalogue = useCallback(async (json: string) => {
    const next = await setStrategyCatalogue(json);
    setView(next);
    return next;
  }, []);

  return (
    <section className="panel strategy-section">
      <h3>Execution strategy</h3>
      {viewError && <ErrorBanner error={viewError} onDismiss={() => setViewError(null)} />}
      {view === null && !viewError && <p className="muted">Reading…</p>}

      {view !== null && (
        <>
          <div className="instructions-subsection">
            <h4>Default strategy</h4>
            <p className="muted">
              Applied to every task whose repository states nothing and that names no model or
              effort of its own. A task, then its repository, then this — each of the three
              falling through independently (ADR-0016).
            </p>
            <GlobalDefaults catalogue={view.catalogue} />
          </div>

          <div className="instructions-subsection">
            <h4>Approval</h4>
            <ApprovalToggle />
          </div>

          <div className="instructions-subsection">
            <h4>Planner</h4>
            <p className="muted">
              What a planned task&rsquo;s strategy run itself costs. It reads the plan, decides
              the model and effort, and writes them back through Rimaia&rsquo;s own MCP server
              — so it wants a cheap model and a short leash.
            </p>
            <PlannerBudgetControls catalogue={view.catalogue} onStore={storeCatalogue} />
          </div>

          <div className="instructions-subsection">
            <h4>Catalogue</h4>
            <p className="muted">
              The model and effort lists every strategy dropdown draws from. <code>id</code>{" "}
              reaches <code>--model</code> or <code>--effort</code> verbatim and{" "}
              <code>label</code> draws the option, so a model Anthropic ships tomorrow is one
              edit here rather than a Rimaia release.
            </p>
            <CatalogueEditor view={view} onStore={storeCatalogue} />
          </div>
        </>
      )}
    </section>
  );
}

/**
 * The global level of the precedence chain.
 *
 * Committed optimistically and reverted on rejection — `set_strategy_defaults`
 * answers with nothing, so unlike the catalogue writes below there is no
 * stored value to repaint from, and the same trade `RunEnvironmentToggle`
 * makes applies here.
 */
function GlobalDefaults({ catalogue }: { readonly catalogue: Catalogue }) {
  const [value, setValue] = useState<StrategyDefaults | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getStrategyDefaults(null).then(setValue, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  function handleChange(next: StrategyDefaults) {
    const previous = value;
    setValue(next);
    setSaving(true);
    setError(null);
    setStrategyDefaults(null, next).then(
      () => setSaving(false),
      (thrown) => {
        setValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  return (
    <>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {value === null ? (
        !error && <p className="muted">Reading…</p>
      ) : (
        <StrategyDefaultsFields
          catalogue={catalogue}
          value={value}
          idPrefix="strategy-global"
          unsetLabel="No default — Claude Code chooses"
          disabled={saving}
          onChange={handleChange}
        />
      )}
      {saving && <span className="muted">Saving…</span>}
    </>
  );
}

/**
 * The stub decision 3 leaves behind, rendered honestly.
 *
 * `automatic` is preselected because that is what an absent
 * `strategy_approval` key reads as on the backend, not because this component
 * guesses — one rule for what "unset" means, in the module that owns the key.
 */
function ApprovalToggle() {
  const [value, setValue] = useState<StrategyApproval | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  useEffect(() => {
    getStrategyApproval().then(setValue, (thrown) => setError(toRimaiaError(thrown)));
  }, []);

  function handleChange(next: StrategyApproval) {
    if (next === value) return;
    const previous = value;
    setValue(next);
    setSaving(true);
    setError(null);
    setStrategyApproval(next).then(
      () => setSaving(false),
      (thrown) => {
        setValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  return (
    <div className="strategy-approval">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      {value === null ? (
        !error && <p className="muted">Reading…</p>
      ) : (
        <div
          role="radiogroup"
          aria-label="Strategy approval"
          className="run-environment-options"
        >
          {APPROVAL_OPTIONS.map((option) => (
            <label key={option.value} className="run-environment-option">
              <input
                type="radio"
                name="strategy-approval"
                value={option.value}
                aria-label={option.label}
                checked={value === option.value}
                disabled={saving}
                onChange={() => handleChange(option.value)}
              />
              <span className="run-environment-option-body">
                <strong>{option.label}</strong>
              </span>
            </label>
          ))}
        </div>
      )}
      {saving && <span className="muted">Saving…</span>}
      <p className="muted">
        Stored, and read by nothing yet — the gate a planned task waits at lands in a later
        PR. Every planned task runs its implementation immediately after planning today,
        whichever of these is selected.
      </p>
    </div>
  );
}

interface CatalogueWriterProps {
  readonly onStore: (json: string) => Promise<StrategyCatalogueView>;
}

/**
 * The planner's own model, effort and turn limit.
 *
 * These live *inside* the catalogue document, so changing one rewrites the
 * whole key — which normalizes the user's key order and indentation to
 * `JSON.stringify`'s. Stated rather than worked around: it is one key with one
 * writer, and the alternative (a surgical text edit of whatever the textarea
 * holds) would be a second, worse JSON parser in the frontend.
 *
 * The selects are controlled straight off the catalogue prop, unlike the
 * panel's task-level dropdowns: `set_strategy_catalogue` answers with the
 * stored document, so a change repaints from its own result rather than
 * waiting for an event round trip, and there is nothing to snap back from.
 */
function PlannerBudgetControls({
  catalogue,
  onStore,
}: CatalogueWriterProps & { readonly catalogue: Catalogue }) {
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [maxTurnsDraft, setMaxTurnsDraft] = useState(String(catalogue.planner.max_turns));

  function store(planner: PlannerBudget) {
    setSaving(true);
    setError(null);
    // Serialized from the parsed catalogue, which is the built-in one when the
    // stored text will not parse — so using these controls on a row someone
    // mangled by hand replaces it with the list the dropdowns have been
    // showing all along. That is the same document the user is looking at, and
    // it is repairable in the textarea below.
    onStore(JSON.stringify({ ...catalogue, planner }, null, 2)).then(
      () => setSaving(false),
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function commitMaxTurns(raw: string) {
    const parsed = Number(raw);
    if (!Number.isInteger(parsed) || parsed < 1) {
      // Nothing legible to send: `JSON.stringify` would emit `null` for a NaN
      // and the refusal would name a type rather than the field. Snap the box
      // back to the stored value instead of storing a limit nobody typed.
      setMaxTurnsDraft(String(catalogue.planner.max_turns));
      return;
    }
    if (parsed === catalogue.planner.max_turns) return;
    store({ ...catalogue.planner, max_turns: parsed });
  }

  return (
    <div className="strategy-planner">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <CatalogueSelect
        id="strategy-planner-model"
        label="Planner model"
        entries={catalogue.models}
        value={catalogue.planner.model ?? null}
        unsetLabel="No model — Claude Code chooses"
        disabled={saving}
        onChange={(model) => store({ ...catalogue.planner, model })}
      />
      <CatalogueSelect
        id="strategy-planner-effort"
        label="Planner effort"
        entries={catalogue.efforts}
        value={catalogue.planner.effort ?? null}
        unsetLabel="No effort — Claude Code chooses"
        disabled={saving}
        onChange={(effort) => store({ ...catalogue.planner, effort })}
      />
      <label htmlFor="strategy-planner-max-turns">
        Planner turn limit
        <input
          id="strategy-planner-max-turns"
          type="number"
          min={1}
          value={maxTurnsDraft}
          disabled={saving}
          onChange={(event) => setMaxTurnsDraft(event.target.value)}
          onBlur={(event) => commitMaxTurns(event.target.value)}
        />
      </label>
      {saving && <span className="muted">Saving…</span>}
    </div>
  );
}

/**
 * The catalogue as text, committed on blur or unmount — the same idiom
 * `BaseInstructionsEditor` and `PlanEditor` use, so leaving Settings never
 * loses an edit.
 *
 * Controlled rather than uncontrolled, which is the one place this differs
 * from those two: "Restore defaults" and a `settings:changed` from another
 * window both replace the text from outside the DOM, and an uncontrolled
 * textarea would keep showing the bytes the user was looking at before.
 *
 * A rejected edit **keeps the draft on screen** beside the parser's message.
 * Reverting it would throw away the JSON someone just typed over a missing
 * brace they can see and fix.
 */
function CatalogueEditor({
  view,
  onStore,
}: CatalogueWriterProps & { readonly view: StrategyCatalogueView }) {
  const [draft, setDraftState] = useState(view.json);
  const draftRef = useRef(view.json);
  const storedRef = useRef(view.json);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);

  function setDraft(next: string) {
    draftRef.current = next;
    setDraftState(next);
  }

  useEffect(() => {
    // A re-read landed. Adopt it only when the box is clean: a half-typed
    // catalogue is not something to lose because another window saved an
    // unrelated setting.
    const dirty = draftRef.current !== storedRef.current;
    storedRef.current = view.json;
    if (!dirty) {
      setDraft(view.json);
    }
  }, [view.json]);

  function commit(value: string) {
    if (value === storedRef.current) return;
    setSaving(true);
    setError(null);
    onStore(value).then(
      (next) => {
        storedRef.current = next.json;
        setDraft(next.json);
        setSaving(false);
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  useEffect(() => {
    // Backstop for leaving Settings without a preceding blur, exactly as
    // `BaseInstructionsEditor` has one. An unparseable draft is refused here
    // and nothing is stored — which is the right outcome: the catalogue that
    // was already valid stays valid, and the refusal has no panel left to
    // render it in.
    return () => {
      if (draftRef.current !== storedRef.current) {
        onStore(draftRef.current).catch(() => {});
      }
    };
    // `onStore` is deliberately not a dependency: it is a `useCallback` over
    // `setView` alone, so listing it would only tear down and rebuild this
    // cleanup on every parent render, for no behavioural difference — the same
    // note `BaseInstructionsEditor` makes about its own backstop.
  }, []);

  return (
    <>
      {saving && <span className="muted">Saving…</span>}
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}
      <textarea
        className="instructions-editor-textarea"
        value={draft}
        aria-label="Strategy catalogue"
        spellCheck={false}
        onChange={(event) => setDraft(event.target.value)}
        onBlur={(event) => commit(event.target.value)}
      />
      <div className="strategy-catalogue-actions">
        <button
          type="button"
          disabled={saving || draft === view.defaultJson}
          onClick={() => commit(view.defaultJson)}
        >
          Restore defaults
        </button>
      </div>
    </>
  );
}
