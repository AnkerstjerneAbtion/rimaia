//! A runner's strategy ceiling: the most expensive model and effort this
//! machine's owner lets a run spend their subscription on (ADR-0032 point 3's
//! last paragraph).
//!
//! A cost control, not consent. The board refuses a claim whose named choice
//! is above the ceiling; the runner judges again at spawn and spawns with the
//! answer, filling a half nothing named. Pure: values in, values out.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::machine::MachineContext;
use crate::strategy::{Catalogue, StrategyOrigin};

/// The runner setting `strategy_ceiling` in `runner.db`'s `runner_settings`,
/// as JSON. Absent, and the default, is no ceiling: every existing install.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StrategyCeiling {
    /// The model ids a run may spawn with. The first fills a phase that names
    /// no model.
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// The most expensive effort a run may spawn with, ranked by its position
    /// in the team's catalogue, which lists efforts cheapest first.
    #[serde(default)]
    pub max_effort: Option<String>,
}

impl StrategyCeiling {
    /// Whether this ceiling constrains anything.
    pub fn is_none(&self) -> bool {
        self.models.is_none() && self.max_effort.is_none()
    }
}

/// The `runner_settings` key holding the ceiling, as [`StrategyCeiling`]'s
/// JSON.
///
/// Read straight through the machine store, like `runner::limits`' two
/// override keys, and for their reason: it is not in
/// `db::settings::RUNNER_KEYS`, because the board's `settings` never held it
/// and task 040's adoption has nothing to copy. [`set_strategy_ceiling`] is
/// its one writer, behind the local commands and tools of the same name.
pub const STRATEGY_CEILING: &str = "strategy_ceiling";

/// This runner's ceiling, read when each run starts by the route
/// `run_environment` takes (task 041), never cached across runs. Absent is no
/// ceiling. Tolerant on read, as every stored value is (ADR-0003): a value
/// that does not parse is logged and read as no ceiling.
pub async fn strategy_ceiling(machine: &MachineContext) -> Result<StrategyCeiling> {
    let Some(stored) = machine.store.get_setting(STRATEGY_CEILING).await? else {
        return Ok(StrategyCeiling::default());
    };
    Ok(serde_json::from_str(&stored).unwrap_or_else(|error| {
        tracing::warn!(value = stored, %error, "unusable strategy_ceiling; no ceiling applies");
        StrategyCeiling::default()
    }))
}

/// Stores this runner's ceiling, replacing the one before it whole. A ceiling
/// with neither half reads back exactly as an absent key: no ceiling.
///
/// Read by the next claim and the next spawn, never by one already judged. A
/// list of no models is refused rather than stored: it would refuse every
/// task that names a model and fill none, which nobody means; `None` is "any
/// model". A blank id is refused for the same reason.
pub async fn set_strategy_ceiling(
    machine: &MachineContext,
    ceiling: &StrategyCeiling,
) -> Result<()> {
    if let Some(models) = &ceiling.models {
        if models.is_empty() {
            return Err(Error::invalid(
                "a strategy ceiling must allow at least one model; leave `models` unset to allow \
                 any model",
            ));
        }
        if models.iter().any(|model| model.trim().is_empty()) {
            return Err(Error::invalid(
                "a strategy ceiling's model ids cannot be blank",
            ));
        }
    }
    if ceiling
        .max_effort
        .as_deref()
        .is_some_and(|effort| effort.trim().is_empty())
    {
        return Err(Error::invalid(
            "a strategy ceiling's highest effort cannot be blank; leave it unset for no limit",
        ));
    }

    let stored = serde_json::to_string(ceiling)
        .map_err(|error| Error::internal(format!("a strategy ceiling must serialize: {error}")))?;
    machine.store.set_setting(STRATEGY_CEILING, &stored).await
}

/// What one phase would spawn with, and where each half came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseStrategy {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub model_origin: StrategyOrigin,
    pub effort_origin: StrategyOrigin,
}

/// What a phase spawns with once the ceiling has judged it: the input
/// unchanged, or with an absent half filled as
/// [`StrategyOrigin::RunnerCeiling`].
pub type CeiledStrategy = PhaseStrategy;

/// A named choice above the ceiling. Never lowered: the person who named it
/// decides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CeilingExceeded {
    Model { model: String },
    Effort { effort: String, max_effort: String },
}

/// The ceiling's rule, for every purpose:
///
/// - **A named choice is never silently changed.** A model not in `models`,
///   or an effort that ranks above `max_effort`, is refused. An effort the
///   catalogue does not list exceeds every ceiling.
/// - **An absent choice is filled.** No model means the first of `models`;
///   no effort means `max_effort`.
pub fn judge(
    strategy: &PhaseStrategy,
    ceiling: &StrategyCeiling,
    catalogue: &Catalogue,
) -> Result<CeiledStrategy, CeilingExceeded> {
    let mut ceiled = strategy.clone();

    if let Some(models) = &ceiling.models {
        match &strategy.model {
            Some(model) if !models.contains(model) => {
                return Err(CeilingExceeded::Model {
                    model: model.clone(),
                });
            }
            Some(_) => {}
            None => {
                if let Some(first) = models.first() {
                    ceiled.model = Some(first.clone());
                    ceiled.model_origin = StrategyOrigin::RunnerCeiling;
                }
            }
        }
    }

    if let Some(max_effort) = &ceiling.max_effort {
        match &strategy.effort {
            Some(effort) => {
                let rank = |id: &str| catalogue.efforts.iter().position(|entry| entry.id == id);
                let within = matches!(
                    (rank(effort), rank(max_effort)),
                    (Some(effort), Some(max)) if effort <= max
                );
                if !within {
                    return Err(CeilingExceeded::Effort {
                        effort: effort.clone(),
                        max_effort: max_effort.clone(),
                    });
                }
            }
            None => {
                ceiled.effort = Some(max_effort.clone());
                ceiled.effort_origin = StrategyOrigin::RunnerCeiling;
            }
        }
    }

    Ok(ceiled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::catalogue::CatalogueEntry;
    use pretty_assertions::assert_eq;

    fn catalogue() -> Catalogue {
        let entries = |ids: &[&str]| {
            ids.iter()
                .map(|id| CatalogueEntry {
                    id: (*id).to_string(),
                    label: (*id).to_string(),
                })
                .collect()
        };
        Catalogue {
            models: entries(&["haiku", "sonnet", "opus"]),
            efforts: entries(&["low", "medium", "high"]),
            ..Catalogue::default()
        }
    }

    fn named(model: Option<&str>, effort: Option<&str>) -> PhaseStrategy {
        PhaseStrategy {
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            model_origin: StrategyOrigin::Task,
            effort_origin: StrategyOrigin::Repository,
        }
    }

    fn ceiling(models: &[&str], max_effort: &str) -> StrategyCeiling {
        StrategyCeiling {
            models: Some(models.iter().map(|id| (*id).to_string()).collect()),
            max_effort: Some(max_effort.to_string()),
        }
    }

    #[test]
    fn a_named_model_outside_the_ceiling_is_refused() {
        assert_eq!(
            judge(
                &named(Some("opus"), None),
                &ceiling(&["haiku", "sonnet"], "high"),
                &catalogue()
            ),
            Err(CeilingExceeded::Model {
                model: "opus".to_string()
            })
        );
    }

    #[test]
    fn an_effort_above_the_ceiling_is_refused_never_lowered() {
        assert_eq!(
            judge(
                &named(Some("sonnet"), Some("high")),
                &ceiling(&["sonnet"], "medium"),
                &catalogue()
            ),
            Err(CeilingExceeded::Effort {
                effort: "high".to_string(),
                max_effort: "medium".to_string(),
            })
        );
        // At the ceiling and below it, the named effort is kept as named.
        for effort in ["low", "medium"] {
            assert_eq!(
                judge(
                    &named(Some("sonnet"), Some(effort)),
                    &ceiling(&["sonnet"], "medium"),
                    &catalogue()
                ),
                Ok(named(Some("sonnet"), Some(effort)))
            );
        }
    }

    #[test]
    fn an_absent_model_and_effort_are_filled_from_the_ceiling() {
        assert_eq!(
            judge(
                &named(None, None),
                &ceiling(&["sonnet", "haiku"], "medium"),
                &catalogue()
            ),
            Ok(PhaseStrategy {
                model: Some("sonnet".to_string()),
                effort: Some("medium".to_string()),
                model_origin: StrategyOrigin::RunnerCeiling,
                effort_origin: StrategyOrigin::RunnerCeiling,
            })
        );
    }

    #[test]
    fn an_effort_the_catalogue_does_not_list_exceeds_every_ceiling() {
        assert_eq!(
            judge(
                &named(None, Some("ultra")),
                &ceiling(&["sonnet"], "high"),
                &catalogue()
            ),
            Err(CeilingExceeded::Effort {
                effort: "ultra".to_string(),
                max_effort: "high".to_string(),
            })
        );
    }

    #[test]
    fn no_ceiling_changes_nothing() {
        for strategy in [
            named(None, None),
            named(Some("opus"), Some("high")),
            named(Some("a-model-no-catalogue-lists"), Some("ultra")),
        ] {
            assert_eq!(
                judge(&strategy, &StrategyCeiling::default(), &catalogue()),
                Ok(strategy.clone())
            );
        }
    }
}
