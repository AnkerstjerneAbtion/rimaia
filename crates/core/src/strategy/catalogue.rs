//! The models and effort levels a task may be given, and the planner's own
//! budget (ADR-0016, seam-contract D17.2).
//!
//! The key constant and its rules live here rather than in
//! [`crate::db::settings`], for the reason [`crate::mcp::settings`] gives at
//! its own: seam-contract D3 puts the rules about a key with the code that has
//! the rules, and nothing outside the strategy module has any business knowing
//! what `strategy_catalogue` means. Storage is still task 006's accessor, so
//! there is one `settings` reader and not two.
//!
//! Unseeded, like `run_environment` and `mcp_port`: an absent key *is*
//! [`Catalogue::default`], and a row only appears once the user has edited it.
//! That is what keeps ADR-0016's "a new model does not require a release" true
//! in both directions — a model Anthropic ships tomorrow is one settings edit
//! away, and a user who has never opened Settings still gets a list that
//! matches the release they installed.
//!
//! [`DEFAULT_CATALOGUE_JSON`] is exported because Settings' "Restore defaults"
//! needs bytes to write, and a test below pins it against
//! [`Catalogue::default`] so the two cannot drift.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};

/// The `settings` key holding the catalogue, as JSON.
pub const STRATEGY_CATALOGUE: &str = "strategy_catalogue";

/// What a planner run is allowed to spend before it is cut off (ADR-0011).
///
/// Six is enough to read a plan, think, and make one tool call, and small
/// enough that a planner stuck in a loop costs cents. It is configuration for
/// the same reason the model list is: the number that bounds a cheap model is
/// not the number that bounds an expensive one.
const DEFAULT_PLANNER_MAX_TURNS: u32 = 6;

/// What "Restore defaults" writes, and what an absent key stands for.
///
/// Duplicated between this constant and [`Catalogue::default`] on purpose —
/// the textarea in Settings edits *text*, and a `serde_json::to_string_pretty`
/// of the Rust value would render with whatever key order and indentation
/// serde happened to choose. A test below parses this into the Rust value and
/// asserts they agree, which is the same pin `DEFAULT_BASE_INSTRUCTIONS` keeps
/// against its migration.
pub const DEFAULT_CATALOGUE_JSON: &str = r#"{
  "models": [
    { "id": "opus", "label": "Opus" },
    { "id": "sonnet", "label": "Sonnet" },
    { "id": "haiku", "label": "Haiku" }
  ],
  "efforts": [
    { "id": "low", "label": "Low" },
    { "id": "medium", "label": "Medium" },
    { "id": "high", "label": "High" },
    { "id": "xhigh", "label": "Extra high" },
    { "id": "max", "label": "Max" }
  ],
  "planner": { "model": "haiku", "effort": "low", "max_turns": 6 }
}"#;

/// One choice in a dropdown.
///
/// `id` reaches `--model` or `--effort` verbatim; `label` draws the option. Two
/// fields rather than one because the flag value and the word a human reads are
/// not the same string and never have been — `xhigh` against "Extra high" — and
/// deriving one from the other would be a presentation rule hidden in a parser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    pub id: String,
    pub label: String,
}

/// The strategy run's own budget, so ADR-0016's "a new model does not require a
/// release" covers the planner too and not only the work it plans.
///
/// `model` and `effort` are optional and default to *absent*, not to the
/// built-in planner: a user who writes `"planner": {}` has said something, and
/// what they have said is "no `--model`, let the CLI choose". The built-in
/// haiku/low pair lives in [`Catalogue::default`], where an *unedited* key
/// reaches it. Explicit beats default, the same rule
/// [`crate::runner::process::disallowed_tools`] states for an empty blocklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlannerBudget {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default = "default_planner_max_turns")]
    pub max_turns: u32,
}

fn default_planner_max_turns() -> u32 {
    DEFAULT_PLANNER_MAX_TURNS
}

impl Default for PlannerBudget {
    fn default() -> Self {
        Self {
            model: Some("haiku".to_string()),
            effort: Some("low".to_string()),
            max_turns: DEFAULT_PLANNER_MAX_TURNS,
        }
    }
}

/// Everything the strategy dropdowns and the planner read.
///
/// `#[serde(default)]` at the container is what makes a *field* someone did not
/// write fall back to the built-in list while an explicitly empty one stays
/// empty. The distinction is deliberate: `"models": []` is an operator saying
/// "offer nothing", which is a thing they are allowed to do, and silently
/// restoring the default would be the same defect
/// [`crate::db::settings::base_instructions`] documents about a field a user
/// cleared.
///
/// `deny_unknown_fields` because this is hand-edited JSON and a misspelled key
/// is otherwise invisible: the whole value falls back to the default anyway
/// (see [`parse`]), so the choice is only between a warning that names the
/// typo and a silence that does not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Catalogue {
    pub models: Vec<CatalogueEntry>,
    pub efforts: Vec<CatalogueEntry>,
    pub planner: PlannerBudget,
}

impl Default for Catalogue {
    fn default() -> Self {
        Self {
            models: entries(&[("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]),
            efforts: entries(&[
                ("low", "Low"),
                ("medium", "Medium"),
                ("high", "High"),
                ("xhigh", "Extra high"),
                ("max", "Max"),
            ]),
            planner: PlannerBudget::default(),
        }
    }
}

fn entries(pairs: &[(&str, &str)]) -> Vec<CatalogueEntry> {
    pairs
        .iter()
        .map(|(id, label)| CatalogueEntry {
            id: (*id).to_string(),
            label: (*label).to_string(),
        })
        .collect()
}

/// The configured catalogue, or [`Catalogue::default`] when the key is absent
/// or holds something unparseable.
///
/// Tolerant rather than fallible, exactly as
/// [`RunEnvironment`](crate::db::RunEnvironment) and
/// [`configured_port`](crate::mcp::configured_port) are, and for the same
/// reason: `settings.value` has no `CHECK` and the user is a supported writer
/// of this file (ADR-0003). A brace hand-deleted in the `sqlite3` CLI costs a
/// log line and the built-in list, never an overnight queue.
pub async fn catalogue(pool: &SqlitePool) -> Result<Catalogue> {
    let Some(stored) = settings::get(pool, STRATEGY_CATALOGUE).await? else {
        return Ok(Catalogue::default());
    };

    Ok(parse(&stored).unwrap_or_else(|message| {
        tracing::warn!(
            error = message,
            "unparseable strategy_catalogue; falling back to the built-in one"
        );
        Catalogue::default()
    }))
}

/// Stores the catalogue as the text the user typed, announcing it as a settings
/// change (ADR-0018).
///
/// Refuses unparseable JSON with serde's own message rather than storing it and
/// letting [`catalogue`] fall back later — the Settings panel renders this
/// sentence inline beside the textarea, and "your edit was accepted and then
/// ignored" is not a thing to make a user discover from a log file.
/// [`Error::invalid`] and no new `ErrorCode` (seam-contract D8).
///
/// The *text* is stored, not a re-serialization of the parsed value: the user's
/// key order and indentation are what they will see when they open Settings
/// again, and what stays legible in the `sqlite3` CLI (ADR-0003).
pub async fn set_catalogue(ctx: &ServiceContext, json: &str) -> Result<()> {
    let trimmed = json.trim();
    parse(trimmed)
        .map_err(|message| Error::invalid(format!("the catalogue is not valid JSON: {message}")))?;

    settings::set(ctx, STRATEGY_CATALOGUE, trimmed).await
}

/// One parser, so the tolerant read and the refusing write cannot disagree
/// about what is valid.
fn parse(stored: &str) -> std::result::Result<Catalogue, String> {
    serde_json::from_str(stored).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn an_unconfigured_catalogue_is_the_built_in_default() {
        let pool = test_pool().await;

        assert_eq!(
            settings::get(&pool, STRATEGY_CATALOGUE)
                .await
                .expect("read the key"),
            None,
            "the key is deliberately unseeded"
        );
        assert_eq!(
            catalogue(&pool).await.expect("read the default"),
            Catalogue::default()
        );
    }

    #[tokio::test]
    async fn a_hand_edited_catalogue_that_is_not_json_falls_back_to_the_default_rather_than_failing(
    ) {
        // The row a user mangled in the sqlite3 CLI. A queue must survive it.
        let h = TestContext::new().await;

        for typo in [
            "",
            "{",
            "[]",
            r#"{"models": "opus"}"#,
            // `deny_unknown_fields` is what turns a plausible misspelling into
            // a warning instead of a silently empty dropdown.
            r#"{"model": [{"id": "opus", "label": "Opus"}]}"#,
        ] {
            settings::set(&h.context, STRATEGY_CATALOGUE, typo)
                .await
                .expect("store a typo");

            assert_eq!(
                catalogue(&h.context.pool).await.expect("read it back"),
                Catalogue::default(),
                "a hand-edited {typo:?} must cost a log line, not a launch"
            );
        }
    }

    #[tokio::test]
    async fn an_explicitly_empty_model_list_means_no_choices_not_the_default_list() {
        // The operator turning a dropdown off is a thing they are allowed to
        // do — `runner::process::disallowed_tools`' established rule. Note that
        // `efforts`, which they did *not* write, still fills in.
        let h = TestContext::new().await;

        settings::set(&h.context, STRATEGY_CATALOGUE, r#"{"models": []}"#)
            .await
            .expect("store an empty model list");

        let stored = catalogue(&h.context.pool).await.expect("read it back");

        assert_eq!(stored.models, Vec::new());
        assert_eq!(stored.efforts, Catalogue::default().efforts);
        assert_eq!(stored.planner, PlannerBudget::default());
    }

    #[tokio::test]
    async fn a_planner_with_no_model_passes_no_model_flag_at_all() {
        // The last row of task 020's absent-value table: an explicit planner
        // object without a `model` means the CLI chooses, not haiku.
        let h = TestContext::new().await;

        settings::set(&h.context, STRATEGY_CATALOGUE, r#"{"planner": {}}"#)
            .await
            .expect("store a planner with no model");

        let planner = catalogue(&h.context.pool)
            .await
            .expect("read it back")
            .planner;

        assert_eq!(planner.model, None);
        assert_eq!(planner.effort, None);
        assert_eq!(
            planner.max_turns, DEFAULT_PLANNER_MAX_TURNS,
            "the turn budget is a safety bound, so it fills in even here"
        );
    }

    #[test]
    fn the_default_catalogue_constant_parses_into_itself() {
        // The pin that keeps the bytes "Restore defaults" writes and the value
        // an unconfigured install reads from drifting apart.
        assert_eq!(
            parse(DEFAULT_CATALOGUE_JSON).expect("the default must parse"),
            Catalogue::default()
        );
    }

    #[test]
    fn the_default_catalogue_is_the_three_models_and_five_efforts_task_020_specifies() {
        // The other half of the pin: the list spelled out here, so adding a
        // model has to be a deliberate edit in two places rather than a typo
        // in one.
        let default = Catalogue::default();

        assert_eq!(
            default
                .models
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["opus", "sonnet", "haiku"]
        );
        assert_eq!(
            default
                .efforts
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
            ["low", "medium", "high", "xhigh", "max"],
            "xhigh and max are new here; the panel's old list stopped at high"
        );
    }

    #[tokio::test]
    async fn an_edited_catalogue_round_trips_as_the_text_that_was_typed() {
        let h = TestContext::new().await;
        let edited = r#"{"models": [{ "id": "opus-5", "label": "Opus 5" }]}"#;

        set_catalogue(&h.context, edited)
            .await
            .expect("store an edited catalogue");

        assert_eq!(
            settings::get(&h.context.pool, STRATEGY_CATALOGUE)
                .await
                .expect("read the row"),
            Some(edited.to_string()),
            "the user's own formatting is what Settings shows them next time"
        );
        assert_eq!(
            catalogue(&h.context.pool)
                .await
                .expect("read it back")
                .models,
            entries(&[("opus-5", "Opus 5")]),
            "a model id nobody compiled is offered without a code change"
        );
    }

    #[tokio::test]
    async fn writing_an_unparseable_catalogue_is_refused_with_the_parser_s_own_message() {
        let h = TestContext::new().await;

        let error = set_catalogue(&h.context, "{ models: [] }")
            .await
            .expect_err("invalid JSON must be refused");

        assert_eq!(
            error.to_string(),
            "the catalogue is not valid JSON: key must be a string at line 1 column 3"
        );
        assert_eq!(
            settings::get(&h.context.pool, STRATEGY_CATALOGUE)
                .await
                .expect("read the key"),
            None,
            "a refused write stores nothing"
        );
    }

    #[tokio::test]
    async fn writing_the_catalogue_publishes_settings() {
        // Settings re-reads on `settings:changed`, which is how a second window
        // learns a model was added (ADR-0018).
        let mut h = TestContext::new().await;

        set_catalogue(&h.context, DEFAULT_CATALOGUE_JSON)
            .await
            .expect("store the default");

        assert_eq!(
            h.changes.try_recv().expect("a publication"),
            crate::ChangeEvent::Settings
        );
    }
}
