import { useEffect, useState } from "react";

import {
  acceptTaskStrategy,
  clearTaskStrategy,
  getStrategyCatalogue,
  planTaskStrategy,
  toRimaiaError,
  updateTask,
} from "../../lib/commands";
import { subscribeToSettingsChanged } from "../../lib/events";
import type {
  Catalogue,
  CatalogueEntry,
  EffectiveStrategyFields,
  RimaiaError,
  StrategyMode,
  StrategyOrigin,
  StrategyPlan,
  StrategySource,
  StrategyWorkflow,
  Task,
} from "../../types";
import { ErrorBanner } from "../ErrorBanner";
import { formatCostUsd } from "./RunOutcomeSection";

/** ADR-0016's three modes, in the words the dropdown shows. */
export const STRATEGY_MODE_LABELS: Record<StrategyMode, string> = {
  default: "Default",
  manual: "Manual",
  planned: "Planned",
};

/**
 * Which link of the precedence chain answered, phrased to be read after
 * "from" — `TaskCard`'s badge title says "Model and effort from the
 * repository default", and this section says "from the repository default"
 * beside the effective value. Exported so the two never drift into two
 * vocabularies for one backend enum.
 */
export const STRATEGY_ORIGIN_LABELS: Record<StrategyOrigin, string> = {
  task: "this task",
  repository: "the repository default",
  global: "the global default",
  claude_code: "Claude Code's own default",
};

const WORKFLOW_LABELS: Record<StrategyWorkflow, string> = {
  single_agent: "One agent, start to finish",
  multi_agent: "Several agents, in phases",
};

/** The `<select>` value standing in for "no override" — never sent as
 *  itself, translated to `null` (Rimaia's own default) at the command
 *  boundary. */
const DEFAULT_VALUE = "";

/**
 * What the dropdowns draw before `get_strategy_catalogue` answers, and what
 * they keep if it never does.
 *
 * Empty rather than a built-in list on purpose: the default catalogue is a
 * Rust constant (`strategy::DEFAULT_CATALOGUE_JSON`), and a second copy of it
 * here would be a second thing to edit when a model ships — the exact failure
 * ADR-0016 makes the catalogue configuration to avoid. A task's own stored
 * value still renders while this is empty; see {@link StrategySelect}.
 */
const EMPTY_CATALOGUE: Catalogue = {
  models: [],
  efforts: [],
  planner: { model: null, effort: null, max_turns: 0 },
};

/**
 * The `strategy_plan` envelope (seam-contract D17.3), or `null` when there is
 * none — or when what is stored is not one.
 *
 * Tolerant on purpose, the same rule the backend applies to the catalogue and
 * to unknown CLI events: the column is `TEXT` with no CHECK, a user with
 * `sqlite3` is a supported writer, and a panel that throws on a malformed
 * envelope would take the whole task with it. Anything unreadable reads as
 * "no proposal recorded", which is also what the board does about it.
 */
export function parseStrategyPlan(text: string | null): StrategyPlan | null {
  if (!text) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const status = (parsed as StrategyPlan).status;
  if (status !== "proposed" && status !== "failed") return null;
  return parsed as StrategyPlan;
}

/**
 * `Sonnet · high`, or `null` when nothing is configured at all — D12's rule
 * that a card renders nothing rather than a badge with nothing true in it.
 *
 * The model id is capitalised and the effort id is not, because they are
 * different kinds of word: `opus`/`sonnet`/`haiku` are product names, while
 * `high`/`xhigh`/`max` are adjectives that would read wrong title-cased
 * ("Xhigh"). Neither is looked up in the catalogue — a card would have to
 * fetch one (D12 keeps the board to a single query), and an id the catalogue
 * no longer lists still has to render, since it is still what a run spawns
 * with.
 */
export function strategyBadgeText(model: string | null, effort: string | null): string | null {
  const parts: string[] = [];
  if (model) parts.push(model.charAt(0).toUpperCase() + model.slice(1));
  if (effort) parts.push(effort);
  return parts.length === 0 ? null : parts.join(" · ");
}

interface StrategySectionProps {
  readonly taskId: string;
  readonly strategyMode: StrategyMode;
  /** What the *card* asks for, not what a run gets: a task in resolved
   *  `default` mode ignores both columns (seam-contract D17.6), which is why
   *  {@link effective} is a separate prop and not derived from these. */
  readonly model: string | null;
  readonly effort: string | null;
  /** The envelope as stored text — parsed here, not by the panel, so one
   *  parser covers the section and the card. */
  readonly strategyPlan: string | null;
  readonly strategySource: StrategySource | null;
  /**
   * What a run would actually spawn with, resolved in Rust. `null` while
   * `get_task` is still in flight — {@link loading} tells the two apart.
   */
  readonly effective: EffectiveStrategyFields | null;
  readonly loading: boolean;
  /** Refetches the owning task's detail: accepting, clearing and re-planning
   *  all change fields this section renders from props. */
  readonly onChanged: () => void;
}

/**
 * ADR-0016's execution-strategy control — mode, model and effort, the
 * planner's proposal, and what to do about it. Replaces task 005's plain
 * model/effort dropdowns, which named this task as their own replacement.
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
 *
 * The *proposal* underneath them is the opposite: it renders straight from
 * props, because this component is not its author. The planner writes it back
 * over MCP while this panel is open, and props are how that arrives.
 *
 * No rule about which model wins is implemented here. Setting a model flips
 * the mode to `manual`, clearing both flips it back to `default`
 * (seam-contract D17.6) — enforced by `tasks::update_task` so the board and
 * the `update_task` MCP tool cannot disagree (ADR-0006). This section sends
 * the value and renders `strategy_mode` off the row that comes back, rather
 * than predicting it.
 */
export function StrategySection({
  taskId,
  strategyMode,
  model,
  effort,
  strategyPlan,
  strategySource,
  effective,
  loading,
  onChanged,
}: StrategySectionProps) {
  const [mode, setMode] = useState(strategyMode);
  const [modelValue, setModelValue] = useState(model ?? DEFAULT_VALUE);
  const [effortValue, setEffortValue] = useState(effort ?? DEFAULT_VALUE);
  // "Edit" on a proposal: the selects the planner owns become the user's for
  // the rest of this panel's life. Not a mode of its own — saving an edited
  // value is `update_task`, which is what actually takes authorship.
  const [editingProposal, setEditingProposal] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [saving, setSaving] = useState(false);
  const catalogue = useStrategyCatalogue();

  const plan = parseStrategyPlan(strategyPlan);
  // A proposal the planner still owns. Accepting it is `strategy_source`
  // flipping to `user` (D17.7) — there is no `accepted` column to read.
  const plannerOwnsProposal = strategySource === "planner";
  const selectsLocked = mode === "planned" && !editingProposal;

  /** Every mutation here answers with the task row it wrote, so the mode
   *  shown afterwards is the backend's own, never a guess at its rule. */
  function applyWrittenTask(updated: Task) {
    setMode(updated.strategyMode);
    setModelValue(updated.model ?? DEFAULT_VALUE);
    setEffortValue(updated.effort ?? DEFAULT_VALUE);
  }

  function handleModeChange(value: string) {
    const next = value as StrategyMode;
    if (next === mode) return;
    const previous = mode;
    setMode(next);
    setSaving(true);
    setError(null);
    updateTask(taskId, { strategyMode: next }).then(
      (updated) => {
        applyWrittenTask(updated);
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setMode(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function handleModelChange(value: string) {
    if (value === modelValue) return;
    const previous = modelValue;
    setModelValue(value);
    setSaving(true);
    setError(null);
    updateTask(taskId, { model: value === DEFAULT_VALUE ? null : value }).then(
      (updated) => {
        applyWrittenTask(updated);
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setModelValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function handleEffortChange(value: string) {
    if (value === effortValue) return;
    const previous = effortValue;
    setEffortValue(value);
    setSaving(true);
    setError(null);
    updateTask(taskId, { effort: value === DEFAULT_VALUE ? null : value }).then(
      (updated) => {
        applyWrittenTask(updated);
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setEffortValue(previous);
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function handleAccept() {
    setSaving(true);
    setError(null);
    acceptTaskStrategy(taskId).then(
      (updated) => {
        applyWrittenTask(updated);
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  /** Opens the planner's own values for editing, seeded from the row rather
   *  than from whatever this component was mounted with — the planner may
   *  have written them back since. */
  function handleEdit() {
    setModelValue(model ?? DEFAULT_VALUE);
    setEffortValue(effort ?? DEFAULT_VALUE);
    setEditingProposal(true);
  }

  function handleOverride() {
    setEditingProposal(true);
    handleModeChange("manual");
  }

  function handleReplan() {
    setSaving(true);
    setError(null);
    clearTaskStrategy(taskId).then(
      (updated) => {
        applyWrittenTask(updated);
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  function handlePlanNow() {
    setSaving(true);
    setError(null);
    // Resolves as soon as the planner is under way, not once it finishes —
    // the proposal itself arrives on `tasks:changed`, written back through
    // Rimaia's own MCP server.
    planTaskStrategy(taskId).then(
      () => {
        setSaving(false);
        onChanged();
      },
      (thrown) => {
        setError(toRimaiaError(thrown));
        setSaving(false);
      },
    );
  }

  return (
    <section className="task-detail-section strategy-section">
      <h4>Execution strategy</h4>
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      <div className="model-effort-row">
        <label>
          Mode
          <select
            value={mode}
            disabled={saving}
            onChange={(event) => handleModeChange(event.target.value)}
          >
            {(Object.keys(STRATEGY_MODE_LABELS) as StrategyMode[]).map((value) => (
              <option key={value} value={value}>
                {STRATEGY_MODE_LABELS[value]}
              </option>
            ))}
          </select>
        </label>
        <span className="strategy-effective muted">
          {loading || effective === null ? "Loading…" : effectiveSummary(effective)}
        </span>
      </div>

      <div className="model-effort-row">
        <StrategySelect
          label="Model"
          value={modelValue}
          options={catalogue.models}
          disabled={selectsLocked || saving}
          onChange={handleModelChange}
        />
        <StrategySelect
          label="Effort"
          value={effortValue}
          options={catalogue.efforts}
          disabled={selectsLocked || saving}
          onChange={handleEffortChange}
        />
        {saving && <span className="muted">Saving…</span>}
      </div>
      {selectsLocked && (
        <p className="strategy-hint muted">
          Chosen by the planner. Use Edit below to change them yourself.
        </p>
      )}

      {mode === "planned" && plan === null && (
        <p className="strategy-hint muted">
          The planner runs once, before this task's next run, and proposes a model and effort
          here.
        </p>
      )}

      {plan?.status === "proposed" && (
        <StrategyProposal
          plan={plan}
          pendingAcceptance={plannerOwnsProposal}
          busy={saving}
          onAccept={handleAccept}
          onEdit={handleEdit}
          onOverride={handleOverride}
          onReplan={handleReplan}
        />
      )}

      {plan?.status === "failed" && (
        <div className="strategy-failure">
          <p className="strategy-failure-message">
            {`The planner did not produce a strategy (${plan.run?.error ?? "no reason recorded"}). This task runs on the default strategy.`}
          </p>
          <button type="button" disabled={saving} onClick={handlePlanNow}>
            Plan now
          </button>
        </div>
      )}
    </section>
  );
}

/** What a run would spawn with, and who said so. */
function effectiveSummary(effective: EffectiveStrategyFields): string {
  const origin = STRATEGY_ORIGIN_LABELS[effective.effectiveOrigin];
  const badge = strategyBadgeText(effective.effectiveModel, effective.effectiveEffort);
  return badge === null
    ? `Runs with no model or effort flag — ${origin} decides.`
    : `Runs as ${badge} — from ${origin}.`;
}

interface StrategySelectProps {
  readonly label: string;
  readonly value: string;
  readonly options: readonly CatalogueEntry[];
  readonly disabled: boolean;
  readonly onChange: (value: string) => void;
}

/** One catalogue-backed dropdown, plus the hint a value the catalogue does
 *  not list needs. */
function StrategySelect({ label, value, options, disabled, onChange }: StrategySelectProps) {
  // A stored id the catalogue no longer lists is still passed to the CLI
  // verbatim (seam-contract D17.1), so it stays here as the selected option.
  // Dropping it would leave the select re-reading as "Default" — the one
  // rendering that misreports what the task is actually going to do.
  const offCatalogue =
    value !== DEFAULT_VALUE && !options.some((option) => option.id === value);

  return (
    <div className="strategy-field">
      {/* The hint sits outside the `<label>`: everything inside one is part
          of the control's accessible name, and "Model not in the catalogue…"
          is not what this select is called. */}
      <label>
        {label}
        <select value={value} disabled={disabled} onChange={(event) => onChange(event.target.value)}>
          <option value={DEFAULT_VALUE}>Default</option>
          {offCatalogue && <option value={value}>{value}</option>}
          {options.map((option) => (
            <option key={option.id} value={option.id}>
              {option.label}
            </option>
          ))}
        </select>
      </label>
      {offCatalogue && (
        <p className="strategy-hint muted">{`“${value}” is not in the catalogue — a run still passes it verbatim.`}</p>
      )}
    </div>
  );
}

interface StrategyProposalProps {
  readonly plan: StrategyPlan;
  /** The planner still owns this proposal — nobody has accepted or edited it
   *  (`strategy_source` is still `planner`). */
  readonly pendingAcceptance: boolean;
  readonly busy: boolean;
  readonly onAccept: () => void;
  readonly onEdit: () => void;
  readonly onOverride: () => void;
  readonly onReplan: () => void;
}

/**
 * What the planner decided and why, and the four things to do about it.
 *
 * The phases table is read-only: ADR-0016 is explicit that Rimaia injects the
 * decision and never orchestrates the agents, so there is nothing here to
 * schedule or tick off — the implementation run receives these phases as
 * prompt guidance and runs them itself.
 */
function StrategyProposal({
  plan,
  pendingAcceptance,
  busy,
  onAccept,
  onEdit,
  onOverride,
  onReplan,
}: StrategyProposalProps) {
  const phases = plan.phases ?? [];

  return (
    <div className="strategy-proposal">
      <p className="strategy-proposal-status muted">
        {pendingAcceptance
          ? "Proposed by the planner — not accepted yet."
          : "Accepted — this strategy is yours now."}
      </p>
      {plan.rationale && <p className="strategy-rationale">{plan.rationale}</p>}

      <dl className="detail-list">
        <dt>Workflow</dt>
        <dd>{WORKFLOW_LABELS[plan.workflow ?? "single_agent"]}</dd>
        <dt>Proposed</dt>
        <dd>{strategyBadgeText(plan.model ?? null, plan.effort ?? null) ?? "No flags"}</dd>
        <dt>Planner</dt>
        {/* The strategy run gets no `runs` row (D17.5), so its own turns and
            cost are carried inside the envelope; there is nothing else to
            read them off. */}
        <dd>
          {plan.run?.num_turns ?? "—"} turns ·{" "}
          {plan.run?.cost_usd != null ? formatCostUsd(plan.run.cost_usd) : "cost not recorded"}
        </dd>
      </dl>

      {phases.length > 0 && (
        <table className="strategy-phases">
          <thead>
            <tr>
              <th scope="col">Phase</th>
              <th scope="col">Model</th>
              <th scope="col">Effort</th>
              <th scope="col">Agents</th>
            </tr>
          </thead>
          <tbody>
            {phases.map((phase, index) => (
              // Indexed because a planner is free to name two phases the
              // same, and this table is never reordered.
              <tr key={`${index}-${phase.name}`}>
                <td>
                  {phase.name}
                  {phase.summary && <span className="muted"> — {phase.summary}</span>}
                </td>
                <td>{phase.model ?? "—"}</td>
                <td>{phase.effort ?? "—"}</td>
                <td>{phase.agents}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}

      <div className="strategy-actions">
        {pendingAcceptance && (
          <button type="button" disabled={busy} onClick={onAccept}>
            Accept
          </button>
        )}
        <button type="button" disabled={busy} onClick={onEdit}>
          Edit
        </button>
        <button type="button" disabled={busy} onClick={onOverride}>
          Override
        </button>
        <button type="button" disabled={busy} onClick={onReplan}>
          Re-plan
        </button>
      </div>
    </div>
  );
}

/**
 * The model and effort lists the dropdowns draw, re-read on
 * `settings:changed` — adding a model is a settings write, and a panel open
 * in another window has to learn about it (ADR-0016's "a new model must not
 * require a release").
 *
 * A read that fails is not an error banner: the catalogue only populates two
 * dropdowns, the task's own stored values still render without it (they are
 * rendered as off-catalogue options, which is what an unread catalogue makes
 * them), and there is nothing the user did here to be told about.
 */
function useStrategyCatalogue(): Catalogue {
  const [catalogue, setCatalogue] = useState<Catalogue>(EMPTY_CATALOGUE);

  useEffect(() => {
    let active = true;
    let unlisten: (() => void) | undefined;

    function load() {
      getStrategyCatalogue().then(
        (view) => {
          if (active) setCatalogue(view.catalogue);
        },
        () => {
          // No event bridge, or the read itself failed — `EMPTY_CATALOGUE`
          // still offers "Default" and whatever the task already names.
        },
      );
    }

    load();

    subscribeToSettingsChanged(load).then(
      (fn) => {
        if (active) {
          unlisten = fn;
        } else {
          fn();
        }
      },
      () => {
        // No event bridge (tests, or a non-Tauri preview) — the dropdowns
        // still show whatever the initial `load()` above resolved.
      },
    );

    return () => {
      active = false;
      unlisten?.();
    };
  }, []);

  return catalogue;
}
