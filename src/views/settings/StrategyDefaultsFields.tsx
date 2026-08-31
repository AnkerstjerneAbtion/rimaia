import type { Catalogue, CatalogueEntry, StrategyDefaults, StrategyMode } from "../../types";

/**
 * The three modes, spelled for a *default* rather than for a card.
 *
 * `default` is how a level says "no opinion" — the enum has no `inherit`
 * variant, because the column it mirrors is `NOT NULL DEFAULT 'default'`, so
 * fall-through is what `default` means at every level (seam-contract D17.6).
 * The labels say so out loud: a reader who picks "Manual" here and sees a task
 * they never touched change model has to be able to find out why from the
 * control itself.
 */
const MODE_OPTIONS: ReadonlyArray<{ readonly value: StrategyMode; readonly label: string }> = [
  { value: "default", label: "Default — no opinion" },
  { value: "manual", label: "Manual — use the model and effort chosen here" },
  { value: "planned", label: "Planned — a planner run decides" },
];

/** The `<select>` value standing in for "nothing chosen at this level",
 *  translated to `null` at the command boundary. An empty string is free to
 *  mean this because it is not a legal `--model` or `--effort` value, so no
 *  catalogue entry can ever collide with it. */
const UNSET_VALUE = "";

interface StrategyDefaultsFieldsProps {
  readonly catalogue: Catalogue;
  readonly value: StrategyDefaults;
  /** Distinguishes one row's three controls from the next's — the repository
   *  list renders this component once per repository, and duplicate `id`s
   *  would point every label at the first row's `<select>`. */
  readonly idPrefix: string;
  /** What "nothing chosen here" falls through to, which is the one thing that
   *  differs between the two levels: the global default falls through to
   *  Claude Code's own choice, a repository's to the global default. The
   *  sentence differs; the control does not, because the precedence chain
   *  behind both is one function (`strategy::effective_strategy`). */
  readonly unsetLabel: string;
  readonly disabled?: boolean;
  readonly onChange: (next: StrategyDefaults) => void;
}

/**
 * One level of ADR-0016's precedence chain, as three controls.
 *
 * Rendered by Settings for the global default and by the repository row for a
 * per-repository one. Shared rather than written twice because the two levels
 * are the same struct, stored under two keys by the same writer
 * (`strategy::settings`) — two copies of these dropdowns would be two places
 * for "a value the catalogue no longer lists" to be handled differently.
 *
 * Presentational on purpose: it reads nothing and writes nothing. Whoever owns
 * the level owns the round trip, because the global default and a repository's
 * differ in exactly one argument and in what a rejection means for the row it
 * belongs to.
 */
export function StrategyDefaultsFields({
  catalogue,
  value,
  idPrefix,
  unsetLabel,
  disabled,
  onChange,
}: StrategyDefaultsFieldsProps) {
  return (
    <div className="strategy-defaults">
      <label htmlFor={`${idPrefix}-mode`}>
        Mode
        <select
          id={`${idPrefix}-mode`}
          value={value.mode}
          disabled={disabled}
          onChange={(event) => onChange({ ...value, mode: event.target.value as StrategyMode })}
        >
          {MODE_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
      </label>

      <CatalogueSelect
        id={`${idPrefix}-model`}
        label="Model"
        entries={catalogue.models}
        value={value.model ?? null}
        unsetLabel={unsetLabel}
        disabled={disabled}
        onChange={(model) => onChange({ ...value, model })}
      />

      <CatalogueSelect
        id={`${idPrefix}-effort`}
        label="Effort"
        entries={catalogue.efforts}
        value={value.effort ?? null}
        unsetLabel={unsetLabel}
        disabled={disabled}
        onChange={(effort) => onChange({ ...value, effort })}
      />
    </div>
  );
}

interface CatalogueSelectProps {
  readonly id: string;
  readonly label: string;
  readonly entries: CatalogueEntry[];
  readonly value: string | null;
  readonly unsetLabel: string;
  readonly disabled?: boolean;
  readonly onChange: (next: string | null) => void;
}

/**
 * A dropdown over one catalogue list, with the rule that makes an editable
 * catalogue safe: **a stored value the catalogue no longer lists is kept and
 * shown with a hint**, never silently dropped.
 *
 * Deleting "opus" from the catalogue must not rewrite every task that names
 * it — the backend spawns a stored id verbatim whether or not the list still
 * carries it (seam-contract D17.2), so a control that could only render
 * catalogue members would show a lie about what the next run will do.
 */
export function CatalogueSelect({
  id,
  label,
  entries,
  value,
  unsetLabel,
  disabled,
  onChange,
}: CatalogueSelectProps) {
  const offCatalogue = value !== null && !entries.some((entry) => entry.id === value);

  return (
    <label htmlFor={id}>
      {label}
      <select
        id={id}
        value={value ?? UNSET_VALUE}
        disabled={disabled}
        onChange={(event) =>
          onChange(event.target.value === UNSET_VALUE ? null : event.target.value)
        }
      >
        <option value={UNSET_VALUE}>{unsetLabel}</option>
        {entries.map((entry) => (
          <option key={entry.id} value={entry.id}>
            {entry.label}
          </option>
        ))}
        {offCatalogue && <option value={value}>{value} — not in the catalogue</option>}
      </select>
    </label>
  );
}
