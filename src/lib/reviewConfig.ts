import type { FixSession, ReviewConfig, ReviewEnabled, ReviewLevel } from "../types";

/**
 * The pure half of the review loop's configuration controls (task 037).
 *
 * Nothing here resolves the precedence chain. Every `Inherit (<value>)` is
 * read off {@link ReviewLevel.inherited}, which the backend computed; a
 * TypeScript copy of task → repository → global → built-in would be a second
 * implementation free to disagree with the one the runner obeys.
 */

/** The only spelling of "on" a door accepts (task 021, following D20's
 *  `on_done_acknowledged`). The type has no `true` and no `"on"`, and nothing
 *  in `src/` sends one. */
export const ON: ReviewEnabled = "on_cost_acknowledged";

/** Fields a level can decide. Written as a list so a field added to
 *  `ReviewConfig` has to be added here to be edited. */
export type ReviewField = keyof ReviewConfig;

/** What the three-way `enabled` control offers at the repository and task
 *  levels. The global level has a checkbox: on, or off. */
export type EnabledChoice = "inherit" | "off" | "on";

/** What choosing something does. A choice that makes the loop effectively on
 *  does not write: it opens the cost acknowledgement, and `config` is what the
 *  acknowledgement writes. */
export type EnabledOutcome =
  | { readonly kind: "none" }
  | { readonly kind: "write"; readonly config: ReviewConfig }
  | { readonly kind: "acknowledge"; readonly config: ReviewConfig };

/** `config` without `field`: the level stops setting it and inherits. */
export function withoutField(config: ReviewConfig, field: ReviewField): ReviewConfig {
  const { [field]: _removed, ...rest } = config;
  void _removed;
  return rest;
}

/** `config` with `field` set. */
export function withField<K extends ReviewField>(
  config: ReviewConfig,
  field: K,
  value: ReviewConfig[K],
): ReviewConfig {
  return value === undefined ? withoutField(config, field) : { ...config, [field]: value };
}

/**
 * What choosing `choice` for `enabled` does at a level.
 *
 * "Effectively on" is the rule: choosing `On`, and choosing `Inherit` when the
 * level above resolves to on, both turn the loop on at this level. Neither
 * writes until the cost has been acknowledged, because an acknowledgement
 * given at an inherited level was given for that level's scope, not as a
 * promise about a task someone later singled out as `Off`. Everything that
 * leaves the loop effectively off writes at once: nothing about it costs more.
 */
export function enabledOutcome(choice: EnabledChoice, level: ReviewLevel): EnabledOutcome {
  const stored = level.config.enabled;
  const storedChoice: EnabledChoice =
    stored === undefined ? "inherit" : stored === ON ? "on" : "off";
  if (choice === storedChoice) return { kind: "none" };

  switch (choice) {
    case "off":
      return { kind: "write", config: withField(level.config, "enabled", "off") };
    case "on":
      return { kind: "acknowledge", config: withField(level.config, "enabled", ON) };
    case "inherit": {
      const config = withoutField(level.config, "enabled");
      return level.inherited.enabled === ON
        ? { kind: "acknowledge", config }
        : { kind: "write", config };
    }
  }
}

/** The global checkbox: checking opens the acknowledgement, unchecking
 *  writes `off` at once. */
export function globalEnabledOutcome(checked: boolean, level: ReviewLevel): EnabledOutcome {
  const on = level.config.enabled === ON;
  if (checked === on) return { kind: "none" };
  return checked
    ? { kind: "acknowledge", config: withField(level.config, "enabled", ON) }
    : { kind: "write", config: withField(level.config, "enabled", "off") };
}

/** The `Inherit (<value>)` text for one field, from what the level above
 *  resolved to. A model or effort nobody names is the task's own strategy. */
export function inheritedValueText(field: ReviewField, inherited: ReviewConfig): string {
  switch (field) {
    case "enabled":
      return inherited.enabled === ON ? "on" : "off";
    case "max_review_loops":
      return String(inherited.max_review_loops ?? "");
    case "blocking_severity":
      return inherited.blocking_severity ?? "";
    case "review_model":
      return inherited.review_model ?? "the task's own strategy";
    case "review_effort":
      return inherited.review_effort ?? "the task's own strategy";
    case "fix_session":
      return fixSessionLabel(inherited.fix_session);
  }
}

export function fixSessionLabel(session: FixSession | undefined): string {
  return session === "resume" ? "resume the implementation's session" : "a fresh session";
}

/** `0` to `5`, the bound core enforces; the control offers exactly these. */
export const MAX_REVIEW_LOOP_OPTIONS = [0, 1, 2, 3, 4, 5] as const;

export function maxReviewLoopsLabel(count: number): string {
  return count === 0 ? "0 — Review only, no fixes" : String(count);
}
