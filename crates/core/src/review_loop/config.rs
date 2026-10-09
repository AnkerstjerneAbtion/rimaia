//! What the review loop is set to, at three levels, and what a task ends up
//! with (ADR-0017, task 021).
//!
//! # Where each level is stored
//!
//! ```text
//! settings["review_instructions"]   the global review instructions, text
//! settings["review_config"]         the global ReviewConfig, JSON
//! repositories.review_config        one repository's ReviewConfig, JSON
//! tasks.review_config               one task's ReviewConfig, JSON
//! tasks.review_instructions         one task's override of the instructions
//! ```
//!
//! The three columns ride in task 035's migration (seam-contract D28). The
//! two keys are this module's, in D3's shape, and both are **team** placement
//! (D28 point 4, ADR-0028 point 2): they decide what a whole team's runs cost
//! and how they are judged, so they move to the server with the board. No row
//! is seeded, because an absent key is "off".
//!
//! # Precedence is field by field
//!
//! Task, then repository, then global, then the built-in defaults: off, two
//! fixes, `medium`, the task's own strategy, a fresh fix session. A task that
//! sets only `max_review_loops` inherits whether the loop is on at all.
//!
//! # Enabling is an acknowledgement, and the spelling is the record
//!
//! There is no boolean. `on_cost_acknowledged` is the only "on", following
//! D20's `on_done_acknowledged`: the loop multiplies what every task costs, and
//! a value that reads "on" without saying so would be a spend nobody agreed to.
//! A door given `true` or `"on"` refuses it (D8's `Invalid`). A stored value
//! that does not parse — a hand-edited `true` — logs a warning and reads as
//! absent, which is off: D17.2's tolerance rule, and it keeps a typo from
//! enabling a spend.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::review::findings::FindingSeverity;
use crate::runner::provider::AgentProvider;
use crate::strategy::catalogue::{self, Catalogue, CatalogueEntry};

/// The global review instructions (ADR-0017), beside ADR-0009's base
/// instructions. Team placement.
pub const REVIEW_INSTRUCTIONS: &str = "review_instructions";

/// The global [`ReviewConfig`], as JSON. Team placement.
pub const REVIEW_CONFIG: &str = "review_config";

/// "Bounded" is the ADR's word, and five fixes is already a night.
pub const MAX_REVIEW_LOOPS: u32 = 5;

/// What an unset `max_review_loops` means: at most three reviews and two
/// fixes.
pub const DEFAULT_MAX_REVIEW_LOOPS: u32 = 2;

/// What an unset `blocking_severity` means.
pub const DEFAULT_BLOCKING_SEVERITY: FindingSeverity = FindingSeverity::Medium;

/// Whether the loop runs. See this module's header for why "on" is spelled
/// as an acknowledgement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReviewEnabled {
    Off,
    OnCostAcknowledged,
}

/// Whether a fix opens a new session or continues the implementation's.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FixSession {
    #[default]
    Fresh,
    /// The newest implementation row's session, never the review's
    /// (seam-contract D29 point 3).
    Resume,
}

/// One level's settings. Every field is optional, and an absent one inherits.
///
/// Its serde shape is the stored document and the wire shape at once, for the
/// reason `StrategyDefaults` gives: a projection would be a second spelling of
/// one document. `deny_unknown_fields`, so a misspelled key at a door is
/// refused rather than ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<ReviewEnabled>,
    /// How many fix phases one loop may spend, `0..=5`. A fix is always
    /// followed by a review, so `2` is at most three reviews; `0` with the loop
    /// on is ADR-0017's report-only mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_review_loops: Option<u32>,
    /// The least severity a finding needs to start a fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_severity: Option<FindingSeverity>,
    /// The review phase's model and effort. Absent means the task's own
    /// effective strategy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_session: Option<FixSession>,
}

impl ReviewConfig {
    fn is_empty(&self) -> bool {
        *self == ReviewConfig::default()
    }

    /// A stored document, tolerantly: one that does not parse, or whose
    /// `max_review_loops` is out of range, reads as absent with a warning.
    pub fn from_stored(stored: Option<&str>) -> Self {
        let Some(stored) = stored.filter(|text| !text.trim().is_empty()) else {
            return Self::default();
        };
        match serde_json::from_str::<ReviewConfig>(stored) {
            Ok(mut config) => {
                if config
                    .max_review_loops
                    .is_some_and(|loops| loops > MAX_REVIEW_LOOPS)
                {
                    tracing::warn!(
                        value = stored,
                        "a stored max_review_loops above {MAX_REVIEW_LOOPS}; ignoring it"
                    );
                    config.max_review_loops = None;
                }
                config
            }
            Err(error) => {
                tracing::warn!(
                    value = stored,
                    %error,
                    "unparseable review_config; reading it as nothing set, which is off"
                );
                Self::default()
            }
        }
    }

    /// What a door was handed, strictly: `null` is nothing set, and anything
    /// that is not a [`ReviewConfig`] within its bounds is `Invalid`.
    ///
    /// Takes the raw JSON rather than the type so that every door — the
    /// command, the tool, and this function called directly — refuses `true`
    /// in the same sentence, rather than one of them failing in its
    /// framework's deserializer first.
    pub fn from_door(value: serde_json::Value, catalogue: &Catalogue) -> Result<Self> {
        if value.is_null() {
            return Ok(Self::default());
        }
        if let Some(enabled) = value.get("enabled") {
            let spelled = enabled.as_str();
            if !matches!(spelled, Some("off" | "on_cost_acknowledged")) {
                return Err(Error::invalid(format!(
                    "`enabled` must be \"off\" or \"on_cost_acknowledged\", not {enabled}: the \
                     review loop multiplies what every task costs, so turning it on is spelled \
                     as acknowledging that"
                )));
            }
        }
        let config: ReviewConfig = serde_json::from_value(value).map_err(|error| {
            Error::invalid(format!("the review configuration was refused: {error}"))
        })?;

        if let Some(loops) = config.max_review_loops {
            if loops > MAX_REVIEW_LOOPS {
                return Err(Error::invalid(format!(
                    "max_review_loops is at most {MAX_REVIEW_LOOPS}; {loops} fixes is not a \
                     bounded loop"
                )));
            }
        }
        if let Some(model) = &config.review_model {
            ensure_listed("review_model", model, &catalogue.models, "models")?;
        }
        if let Some(effort) = &config.review_effort {
            ensure_listed("review_effort", effort, &catalogue.efforts, "effort levels")?;
        }
        Ok(config)
    }

    /// The stored text, or `None` when nothing is set, so a cleared level is a
    /// NULL column rather than `{}`.
    fn to_stored(&self) -> Result<Option<String>> {
        if self.is_empty() {
            return Ok(None);
        }
        serde_json::to_string(self).map(Some).map_err(|error| {
            Error::internal(format!(
                "the review configuration did not serialize: {error}"
            ))
        })
    }
}

/// A model or effort must be one the catalogue offers: the id is the exact
/// string that reaches the CLI, and one nobody listed is one this
/// installation has not been told about.
fn ensure_listed(field: &str, id: &str, entries: &[CatalogueEntry], noun: &str) -> Result<()> {
    if entries.iter().any(|entry| entry.id == id) {
        return Ok(());
    }
    let listed: Vec<&str> = entries.iter().map(|entry| entry.id.as_str()).collect();
    Err(Error::invalid(format!(
        "{field} \"{id}\" is not in the strategy catalogue's {noun} ({})",
        if listed.is_empty() {
            "it lists none".to_string()
        } else {
            listed.join(", ")
        }
    )))
}

/// A task's loop settings after the precedence chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveReviewConfig {
    pub enabled: bool,
    pub max_review_loops: u32,
    pub blocking_severity: FindingSeverity,
    /// `None` is the task's own effective strategy, which the runner fills in.
    pub review_model: Option<String>,
    pub review_effort: Option<String>,
    pub fix_session: FixSession,
}

impl Default for EffectiveReviewConfig {
    /// The built-in defaults: what a fresh database means.
    fn default() -> Self {
        effective(
            &ReviewConfig::default(),
            &ReviewConfig::default(),
            &ReviewConfig::default(),
        )
    }
}

/// ADR-0017's precedence, field by field: task, repository, global, default.
pub fn effective(
    task: &ReviewConfig,
    repository: &ReviewConfig,
    global: &ReviewConfig,
) -> EffectiveReviewConfig {
    let levels = [task, repository, global];

    EffectiveReviewConfig {
        enabled: levels.iter().find_map(|level| level.enabled)
            == Some(ReviewEnabled::OnCostAcknowledged),
        max_review_loops: levels
            .iter()
            .find_map(|level| level.max_review_loops)
            .unwrap_or(DEFAULT_MAX_REVIEW_LOOPS),
        blocking_severity: levels
            .iter()
            .find_map(|level| level.blocking_severity)
            .unwrap_or(DEFAULT_BLOCKING_SEVERITY),
        review_model: levels.iter().find_map(|level| level.review_model.clone()),
        review_effort: levels.iter().find_map(|level| level.review_effort.clone()),
        fix_session: levels
            .iter()
            .find_map(|level| level.fix_session)
            .unwrap_or_default(),
    }
}

/// The review instructions a run is composed with: the task's override when it
/// says something, the global text otherwise.
///
/// An override **replaces** rather than adds: an override that added would run
/// two review skills on one change. A blank one falls back, because a cleared
/// field and an absent one are the same thing to a reader.
pub fn effective_instructions(task_override: Option<&str>, global: &str) -> String {
    match task_override {
        Some(text) if !text.trim().is_empty() => text.to_string(),
        _ => global.to_string(),
    }
}

/// The global settings, as the two doors read and write them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSettings {
    /// The global review instructions, unexpanded. Empty when unset: Rimaia
    /// ships no review methodology.
    pub instructions: String,
    pub config: ReviewConfig,
}

/// One task's own review settings, before any inheritance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskReview {
    pub instructions: Option<String>,
    pub config: ReviewConfig,
}

/// What a task's loop is set to, and the instructions it is composed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub config: EffectiveReviewConfig,
    /// [`effective_instructions`], unexpanded.
    pub instructions: String,
    pub task: TaskReview,
}

pub async fn review_instructions(pool: &SqlitePool) -> Result<String> {
    Ok(settings::get(pool, REVIEW_INSTRUCTIONS)
        .await?
        .unwrap_or_default())
}

pub async fn global_config(pool: &SqlitePool) -> Result<ReviewConfig> {
    Ok(ReviewConfig::from_stored(
        settings::get(pool, REVIEW_CONFIG).await?.as_deref(),
    ))
}

pub async fn get_review_settings(pool: &SqlitePool) -> Result<ReviewSettings> {
    Ok(ReviewSettings {
        instructions: review_instructions(pool).await?,
        config: global_config(pool).await?,
    })
}

pub async fn repository_config(pool: &SqlitePool, repository_id: &str) -> Result<ReviewConfig> {
    let stored: Option<Option<String>> = sqlx::query_scalar!(
        "SELECT review_config FROM repositories WHERE id = ?1",
        repository_id,
    )
    .fetch_optional(pool)
    .await?;
    let stored =
        stored.ok_or_else(|| Error::not_found(format!("no repository with id {repository_id}")))?;
    Ok(ReviewConfig::from_stored(stored.as_deref()))
}

pub async fn task_review(pool: &SqlitePool, task_id: &str) -> Result<TaskReview> {
    let row = sqlx::query!(
        "SELECT review_instructions, review_config FROM tasks WHERE id = ?1",
        task_id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no task with id {task_id}")))?;
    Ok(TaskReview {
        instructions: row.review_instructions,
        config: ReviewConfig::from_stored(row.review_config.as_deref()),
    })
}

/// `task_id`'s effective loop settings, read fresh. Called at every phase
/// boundary, so a setting changed mid-loop takes effect at the next one.
pub async fn resolve(pool: &SqlitePool, task_id: &str, repository_id: &str) -> Result<Resolved> {
    let task = task_review(pool, task_id).await?;
    let repository = repository_config(pool, repository_id).await?;
    let global = get_review_settings(pool).await?;

    Ok(Resolved {
        config: effective(&task.config, &repository, &global.config),
        instructions: effective_instructions(task.instructions.as_deref(), &global.instructions),
        task,
    })
}

/// Replaces the global instructions and configuration. `config` is `null` for
/// nothing set.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str()))]
pub async fn set_review_settings(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    instructions: &str,
    config: serde_json::Value,
) -> Result<ReviewSettings> {
    let config =
        ReviewConfig::from_door(config, &catalogue::catalogue(&ctx.pool, provider).await?)?;
    let stored = config.to_stored()?.unwrap_or_else(|| "{}".to_string());

    let mut tx = ctx.pool.begin().await?;
    settings::set_in(&mut *tx, REVIEW_INSTRUCTIONS, instructions).await?;
    settings::set_in(&mut *tx, REVIEW_CONFIG, &stored).await?;
    tx.commit().await?;

    ctx.publish(ChangeEvent::Settings);
    Ok(ReviewSettings {
        instructions: instructions.to_string(),
        config,
    })
}

/// Replaces one repository's configuration. `config` is `null` to inherit
/// everything.
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), repository_id = %repository_id))]
pub async fn set_repository_review_config(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    repository_id: &str,
    config: serde_json::Value,
) -> Result<ReviewConfig> {
    let config =
        ReviewConfig::from_door(config, &catalogue::catalogue(&ctx.pool, provider).await?)?;
    let stored = config.to_stored()?;

    let updated = sqlx::query!(
        "UPDATE repositories SET review_config = ?1 WHERE id = ?2",
        stored,
        repository_id,
    )
    .execute(&ctx.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::not_found(format!(
            "no repository with id {repository_id}"
        )));
    }

    ctx.publish(ChangeEvent::repositories([repository_id.to_string()]));
    Ok(config)
}

/// Replaces one task's override of the instructions and its configuration.
///
/// Not a field of `TaskPatch`: `update_task` is open to a planner's grant for
/// its own task, and a run that could rewrite its own review settings would
/// be marking its own homework (ADR-0021 point 4).
#[tracing::instrument(skip_all, fields(source = ctx.source.as_str(), task_id = %task_id))]
pub async fn set_task_review(
    ctx: &ServiceContext,
    provider: &dyn AgentProvider,
    task_id: &str,
    instructions: Option<String>,
    config: serde_json::Value,
) -> Result<TaskReview> {
    let config =
        ReviewConfig::from_door(config, &catalogue::catalogue(&ctx.pool, provider).await?)?;
    let stored = config.to_stored()?;
    let instructions = instructions.filter(|text| !text.trim().is_empty());
    let now = ctx.clock.now();

    let updated = sqlx::query!(
        "UPDATE tasks SET review_instructions = ?1, review_config = ?2, updated_at = ?3
          WHERE id = ?4",
        instructions,
        stored,
        now,
        task_id,
    )
    .execute(&ctx.pool)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(Error::not_found(format!("no task with id {task_id}")));
    }

    ctx.publish(ChangeEvent::tasks([task_id.to_string()]));
    Ok(TaskReview {
        instructions,
        config,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn level(value: serde_json::Value) -> ReviewConfig {
        serde_json::from_value(value).expect("a review config")
    }

    #[test]
    fn task_config_overrides_repository_overrides_global_field_by_field() {
        let global = level(json!({
            "enabled": "on_cost_acknowledged",
            "max_review_loops": 4,
            "blocking_severity": "low",
            "review_model": "opus",
        }));
        let repository = level(json!({ "max_review_loops": 1, "review_effort": "high" }));
        let task = level(json!({ "max_review_loops": 3, "fix_session": "resume" }));

        assert_eq!(
            effective(&task, &repository, &global),
            EffectiveReviewConfig {
                enabled: true,
                max_review_loops: 3,
                blocking_severity: FindingSeverity::Low,
                review_model: Some("opus".to_string()),
                review_effort: Some("high".to_string()),
                fix_session: FixSession::Resume,
            }
        );
        // A task that says "off" wins over a repository and a global that say on.
        let off = level(json!({ "enabled": "off" }));
        assert!(!effective(&off, &repository, &global).enabled);
    }

    #[test]
    fn nothing_set_anywhere_is_the_built_in_defaults() {
        assert_eq!(
            EffectiveReviewConfig::default(),
            EffectiveReviewConfig {
                enabled: false,
                max_review_loops: 2,
                blocking_severity: FindingSeverity::Medium,
                review_model: None,
                review_effort: None,
                fix_session: FixSession::Fresh,
            }
        );
    }

    #[test]
    fn a_blank_override_falls_back_and_a_written_one_replaces() {
        assert_eq!(effective_instructions(None, "global"), "global");
        assert_eq!(effective_instructions(Some("  \n"), "global"), "global");
        assert_eq!(effective_instructions(Some("mine"), "global"), "mine");
    }

    #[test]
    fn a_stored_document_that_does_not_parse_reads_as_nothing_set() {
        for stored in [
            r#"{"enabled": true}"#,
            r#"{"enabled": "on"}"#,
            "not json",
            r#"{"enabld": "on_cost_acknowledged"}"#,
        ] {
            assert_eq!(
                ReviewConfig::from_stored(Some(stored)),
                ReviewConfig::default(),
                "{stored}"
            );
        }
        assert_eq!(
            ReviewConfig::from_stored(Some(r#"{"max_review_loops": 9, "fix_session": "resume"}"#)),
            ReviewConfig {
                fix_session: Some(FixSession::Resume),
                ..ReviewConfig::default()
            },
            "an out-of-range count is dropped and the rest kept",
        );
    }

    #[test]
    fn a_door_refuses_any_on_but_the_acknowledgement() {
        let catalogue = Catalogue::default();
        for value in [json!(true), json!("on"), json!("yes"), json!(1)] {
            let error = ReviewConfig::from_door(json!({ "enabled": value }), &catalogue)
                .expect_err("only the acknowledgement turns the loop on");
            assert!(matches!(error, Error::Invalid { .. }), "{error:?}");
        }
        assert_eq!(
            ReviewConfig::from_door(json!({ "enabled": "on_cost_acknowledged" }), &catalogue)
                .expect("the acknowledgement")
                .enabled,
            Some(ReviewEnabled::OnCostAcknowledged)
        );
    }

    #[test]
    fn a_cleared_level_is_stored_as_nothing() {
        assert_eq!(
            ReviewConfig::default().to_stored().expect("serializes"),
            None
        );
        assert_eq!(
            level(json!({ "max_review_loops": 0 }))
                .to_stored()
                .expect("serializes"),
            Some(r#"{"max_review_loops":0}"#.to_string())
        );
    }
}
