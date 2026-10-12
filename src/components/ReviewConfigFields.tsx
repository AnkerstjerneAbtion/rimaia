import { useEffect, useState } from "react";

import { CatalogueSelect } from "../views/settings/StrategyDefaultsFields";
import { getRunCostSummary, toRimaiaError } from "../lib/commands";
import { reviewLoopCostNote } from "../lib/review";
import {
  MAX_REVIEW_LOOP_OPTIONS,
  enabledOutcome,
  globalEnabledOutcome,
  inheritedValueText,
  maxReviewLoopsLabel,
  withField,
} from "../lib/reviewConfig";
import type { EnabledChoice, EnabledOutcome, ReviewField } from "../lib/reviewConfig";
import type {
  Catalogue,
  FindingSeverity,
  FixSession,
  ReviewConfig,
  ReviewLevel,
  RimaiaError,
  RunCostSummary,
} from "../types";
import { ErrorBanner } from "./ErrorBanner";

/** Severities as the words a finding wears. A list to offer, never to
 *  compare: which findings block is core's. */
const SEVERITY_OPTIONS: ReadonlyArray<readonly [FindingSeverity, string]> = [
  ["critical", "Critical"],
  ["high", "High"],
  ["medium", "Medium"],
  ["low", "Low"],
];

const UNSET = "";

export type ReviewConfigScope = "global" | "repository" | "task";

interface ReviewConfigFieldsProps {
  readonly scope: ReviewConfigScope;
  /** Distinguishes one level's controls from the next: Settings renders this
   *  once per repository, and duplicate ids would point every label at the
   *  first row. */
  readonly idPrefix: string;
  /** The level's own settings beside what it inherits and what it resolves to,
   *  as the backend answered. */
  readonly level: ReviewLevel;
  /** The model and effort vocabulary (D17); `null` while it loads. */
  readonly catalogue: Catalogue | null;
  /** Replaces the level's whole configuration, resolving once the backend has
   *  kept it and the level has been read again. A rejection is shown here. */
  readonly onChange: (config: ReviewConfig) => Promise<void>;
}

/**
 * The review loop's configuration at one level, in the shape of
 * `OnArchiveFields` and `StrategyDefaultsFields` (task 037).
 *
 * Every field is optional and inherits when absent, field by field. At the
 * repository and task levels each offers `Inherit (<value>)`, and the value is
 * what the level above resolves to **as the backend said so**; the global
 * level offers the built-in default in the same place.
 *
 * # Turning the loop on
 *
 * The loop multiplies what every task costs, so turning it on is spelled as
 * acknowledging that (`enabled: "on_cost_acknowledged"`, task 021). Any choice
 * that makes it effectively on at this level opens the cost, in dollars
 * measured on this installation, and **writes nothing** until `Turn on review
 * loop` is pressed; `Cancel` writes nothing either, and the control keeps
 * showing the stored value throughout. Any choice that leaves it off writes at
 * once, the asymmetry `StorageSection` applies to `worktree_auto_cleanup`.
 *
 * Presentational about storage: whoever owns the level owns the round trip.
 */
export function ReviewConfigFields({
  scope,
  idPrefix,
  level,
  catalogue,
  onChange,
}: ReviewConfigFieldsProps) {
  const [pending, setPending] = useState<ReviewConfig | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<RimaiaError | null>(null);
  const [costs, setCosts] = useState<RunCostSummary | null>(null);

  useEffect(() => {
    // Cosmetic and silent on failure, as every other cost note is: without a
    // measurement the sentence says so rather than guessing a price.
    getRunCostSummary().then(setCosts, () => {});
  }, []);

  const stored = level.config;
  const unsetPrefix = scope === "global" ? "Built-in default" : "Inherit";
  const unsetLabel = (field: ReviewField) =>
    `${unsetPrefix} (${inheritedValueText(field, level.inherited)})`;

  async function write(config: ReviewConfig) {
    setSaving(true);
    setError(null);
    try {
      await onChange(config);
    } catch (thrown) {
      setError(toRimaiaError(thrown));
    } finally {
      setSaving(false);
    }
  }

  function apply(outcome: EnabledOutcome) {
    if (outcome.kind === "write") void write(outcome.config);
    // Nothing is written; the control keeps showing what is stored.
    else if (outcome.kind === "acknowledge") setPending(outcome.config);
  }

  async function acknowledge() {
    const config = pending;
    setPending(null);
    if (config) await write(config);
  }

  const storedChoice: EnabledChoice =
    stored.enabled === undefined ? "inherit" : stored.enabled === "off" ? "off" : "on";
  const effectiveLoops = level.effective.max_review_loops ?? level.inherited.max_review_loops ?? 2;
  const name = `review-enabled-${idPrefix}`;

  return (
    <div className="review-config-fields">
      {error && <ErrorBanner error={error} onDismiss={() => setError(null)} />}

      {scope === "global" ? (
        <label className="review-config-enabled" htmlFor={`${idPrefix}-enabled`}>
          <input
            id={`${idPrefix}-enabled`}
            type="checkbox"
            checked={stored.enabled === "on_cost_acknowledged"}
            disabled={saving}
            onChange={(event) => apply(globalEnabledOutcome(event.target.checked, level))}
          />
          Review each task after it is implemented
        </label>
      ) : (
        <fieldset className="review-config-enabled-choices">
          <legend>Review each task after it is implemented</legend>
          {(
            [
              ["inherit", unsetLabel("enabled")],
              ["off", "Off"],
              ["on", "On"],
            ] as const
          ).map(([choice, label]) => (
            <label key={choice} htmlFor={`${name}-${choice}`} className="review-config-option">
              <input
                id={`${name}-${choice}`}
                type="radio"
                name={name}
                value={choice}
                checked={storedChoice === choice}
                disabled={saving}
                onChange={() => apply(enabledOutcome(choice, level))}
              />
              {label}
            </label>
          ))}
        </fieldset>
      )}

      {pending && (
        <div
          className="review-cost-confirm"
          role="alertdialog"
          aria-label="Confirm turning on the review loop"
        >
          <p>
            Turning this on has a fresh agent review each task after it is implemented, and fix
            what it finds. That multiplies what every task costs.
          </p>
          <p>{reviewLoopCostNote(effectiveLoops, costs)}</p>
          <div className="review-cost-confirm-actions">
            <button type="button" onClick={() => void acknowledge()}>
              Turn on review loop
            </button>
            <button type="button" onClick={() => setPending(null)}>
              Cancel
            </button>
          </div>
        </div>
      )}

      <div className="review-config-grid">
        <label htmlFor={`${idPrefix}-loops`}>
          Fix loops
          <select
            id={`${idPrefix}-loops`}
            value={stored.max_review_loops === undefined ? UNSET : String(stored.max_review_loops)}
            disabled={saving}
            onChange={(event) =>
              void write(
                withField(
                  stored,
                  "max_review_loops",
                  event.target.value === UNSET ? undefined : Number(event.target.value),
                ),
              )
            }
          >
            <option value={UNSET}>{unsetLabel("max_review_loops")}</option>
            {MAX_REVIEW_LOOP_OPTIONS.map((count) => (
              <option key={count} value={count}>
                {maxReviewLoopsLabel(count)}
              </option>
            ))}
          </select>
        </label>

        <label htmlFor={`${idPrefix}-severity`}>
          Blocking severity
          <select
            id={`${idPrefix}-severity`}
            value={stored.blocking_severity ?? UNSET}
            disabled={saving}
            onChange={(event) =>
              void write(
                withField(
                  stored,
                  "blocking_severity",
                  event.target.value === UNSET
                    ? undefined
                    : (event.target.value as FindingSeverity),
                ),
              )
            }
          >
            <option value={UNSET}>{unsetLabel("blocking_severity")}</option>
            {SEVERITY_OPTIONS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>

        <CatalogueSelect
          id={`${idPrefix}-model`}
          label="Review model"
          entries={catalogue?.models ?? []}
          value={stored.review_model ?? null}
          unsetLabel={unsetLabel("review_model")}
          disabled={saving || catalogue === null}
          onChange={(model) => void write(withField(stored, "review_model", model ?? undefined))}
        />

        <CatalogueSelect
          id={`${idPrefix}-effort`}
          label="Review effort"
          entries={catalogue?.efforts ?? []}
          value={stored.review_effort ?? null}
          unsetLabel={unsetLabel("review_effort")}
          disabled={saving || catalogue === null}
          onChange={(effort) =>
            void write(withField(stored, "review_effort", effort ?? undefined))
          }
        />

        <label htmlFor={`${idPrefix}-session`}>
          Fix session
          <select
            id={`${idPrefix}-session`}
            value={stored.fix_session ?? UNSET}
            disabled={saving}
            onChange={(event) =>
              void write(
                withField(
                  stored,
                  "fix_session",
                  event.target.value === UNSET ? undefined : (event.target.value as FixSession),
                ),
              )
            }
          >
            <option value={UNSET}>{unsetLabel("fix_session")}</option>
            <option value="fresh">Fresh session</option>
            <option value="resume">Resume the implementation's session</option>
          </select>
        </label>
      </div>
    </div>
  );
}
