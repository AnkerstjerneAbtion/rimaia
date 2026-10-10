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
//! **What "the default catalogue" is is a provider's answer, not this
//! module's** (task 032, ADR-0026): [`Catalogue::default`] and
//! [`PlannerBudget::default`] are the *neutral* Rust defaults — no models, no
//! opinion — and an installation's actual defaults come from
//! [`AgentProvider::default_catalogue`](crate::runner::provider::AgentProvider::default_catalogue).
//! [`catalogue`] falls back to that when the `strategy_catalogue` key is
//! absent or will not parse.

use serde::{Deserialize, Serialize};

use crate::context::ServiceContext;
use crate::db::settings;
use crate::error::{Error, Result};
use crate::runner::provider::ProviderId;

/// The `settings` key holding the catalogue, as JSON.
pub const STRATEGY_CATALOGUE: &str = "strategy_catalogue";

/// What a planner run is allowed to spend before it is cut off (ADR-0011).
///
/// Six is enough to read a plan, think, and make one tool call, and small
/// enough that a planner stuck in a loop costs cents. It is configuration for
/// the same reason the model list is: the number that bounds a cheap model is
/// not the number that bounds an expensive one.
const DEFAULT_PLANNER_MAX_TURNS: u32 = 6;

/// One choice in a dropdown.
///
/// `id` reaches `--model` or `--effort` verbatim; `label` draws the option. Two
/// fields rather than one because the flag value and the word a human reads are
/// not the same string and never have been — `xhigh` against "Extra high" — and
/// deriving one from the other would be a presentation rule hidden in a parser.
/// `JsonSchema` because ADR-0021 puts this on the tool surface, and because
/// unlike a row type it *is* the wire shape: seam-contract D16.1 keeps row types
/// out of `mcp::responses` by projecting them, but a catalogue is a
/// configuration document whose serde shape is already what gets stored and what
/// the operator edits. A projection here would be a second spelling of one
/// document, free to drift from the thing it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatalogueEntry {
    pub id: String,
    pub label: String,
}

/// The strategy run's own budget, so ADR-0016's "a new model does not require a
/// release" covers the planner too and not only the work it plans.
///
/// `model` and `effort` are optional and default to *absent*, not to a
/// provider's built-in planner: a user who writes `"planner": {}` has said
/// something, and what they have said is "no `--model`, let the CLI choose".
/// The active provider's own pairing — Claude: haiku/low — lives in
/// [`AgentProvider::default_catalogue`](crate::runner::provider::AgentProvider::default_catalogue),
/// where an *unedited* key reaches it; [`PlannerBudget::default`] itself is
/// the neutral value with no opinion, for the reason [`Catalogue::default`]
/// gives. Explicit beats default, the same rule
/// [`crate::board::service::team_disallowed_tools`] states for an empty blocklist.
/// `JsonSchema` because ADR-0021 puts this on the tool surface, and because
/// unlike a row type it *is* the wire shape: seam-contract D16.1 keeps row types
/// out of `mcp::responses` by projecting them, but a catalogue is a
/// configuration document whose serde shape is already what gets stored and what
/// the operator edits. A projection here would be a second spelling of one
/// document, free to drift from the thing it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
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
    /// No model, no effort — the CLI's own default decides. Not a Claude
    /// opinion in disguise: `max_turns` is Rimaia's own safety bound
    /// (ADR-0011), not a model name.
    fn default() -> Self {
        Self {
            model: None,
            effort: None,
            max_turns: DEFAULT_PLANNER_MAX_TURNS,
        }
    }
}

/// Everything the strategy dropdowns and the planner read.
///
/// This is the *resolved* shape — always fully populated, one way or another
/// — which is what [`RawCatalogue`] exists beside it to make possible: telling
/// "the operator wrote nothing here" (fill in from the provider) apart from
/// "the operator wrote an empty list" (leave it empty) needs `Option`, and
/// `#[serde(default)]` on this struct's own fields cannot see which happened
/// once parsing has already collapsed the two.
///
/// `deny_unknown_fields` on [`RawCatalogue`] is what catches a misspelled key
/// in hand-edited JSON — the whole value falls back to the provider's default
/// (see [`catalogue`]), so the choice is only between a warning that names the
/// typo and a silence that does not.
/// `JsonSchema` because ADR-0021 puts this on the tool surface, and because
/// unlike a row type it *is* the wire shape: seam-contract D16.1 keeps row types
/// out of `mcp::responses` by projecting them, but a catalogue is a
/// configuration document whose serde shape is already what gets stored and what
/// the operator edits. A projection here would be a second spelling of one
/// document, free to drift from the thing it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Catalogue {
    pub models: Vec<CatalogueEntry>,
    pub efforts: Vec<CatalogueEntry>,
    pub planner: PlannerBudget,
}

impl Default for Catalogue {
    /// No models, no efforts, the neutral [`PlannerBudget::default`] — the
    /// answer for a Rust value with no provider in front of it. What an
    /// *unconfigured installation* actually offers is
    /// [`AgentProvider::default_catalogue`](crate::runner::provider::AgentProvider::default_catalogue),
    /// which [`catalogue`] reaches for instead of this.
    fn default() -> Self {
        Self {
            models: Vec::new(),
            efforts: Vec::new(),
            planner: PlannerBudget::default(),
        }
    }
}

/// The configured catalogue, or `provider`'s own
/// [`default_catalogue`](crate::runner::provider::AgentProvider::default_catalogue)
/// when the key is absent or holds something unparseable.
///
/// Tolerant rather than fallible, exactly as
/// [`RunEnvironment`](crate::db::RunEnvironment) and
/// [`configured_port`](crate::mcp::configured_port) are, and for the same
/// reason: `settings.value` has no `CHECK` and the user is a supported writer
/// of this file (ADR-0003). A brace hand-deleted in the `sqlite3` CLI costs a
/// log line and the built-in list, never an overnight queue.
///
/// The context's one team's catalogue: what Settings shows and edits.
pub async fn catalogue(
    ctx: &ServiceContext,
    provider: &dyn crate::runner::provider::AgentProvider,
) -> Result<Catalogue> {
    catalogue_for(ctx, ctx.scope.sole()?, provider).await
}

/// One team's catalogue: what a planner for that team's task chooses from,
/// whichever teams the context reaches.
pub async fn catalogue_for(
    ctx: &ServiceContext,
    team_id: &str,
    provider: &dyn crate::runner::provider::AgentProvider,
) -> Result<Catalogue> {
    resolved_over(ctx, team_id, provider.default_catalogue()).await
}

/// [`catalogue_for`], for a caller that holds a provider's identity rather
/// than the provider: selection's plan, whose runner view carries a
/// [`ProviderId`] (task 067). The same document, filled from the same
/// defaults, since [`ProviderId::default_catalogue`] is the provider's own.
pub async fn catalogue_for_provider(
    ctx: &ServiceContext,
    team_id: &str,
    provider: ProviderId,
) -> Result<Catalogue> {
    resolved_over(ctx, team_id, provider.default_catalogue()).await
}

/// The team's stored catalogue filled from `default`, or `default` itself
/// when the key is absent or will not parse.
async fn resolved_over(ctx: &ServiceContext, team_id: &str, default: Catalogue) -> Result<Catalogue> {
    let Some(stored) = settings::get_team(ctx, team_id, STRATEGY_CATALOGUE).await? else {
        return Ok(default);
    };

    Ok(match parse_raw(&stored) {
        Ok(raw) => resolve(raw, default),
        Err(message) => {
            tracing::warn!(
                error = message,
                "unparseable strategy_catalogue; falling back to the built-in one"
            );
            default
        }
    })
}

/// Whether a runner whose provider is `provider` can run `model`, given the
/// catalogue the board resolves for that provider (ADR-0031 point 1, task
/// 067's model rule).
///
/// `false` only when `model` is **not** an id in `resolved` **and** it **is**
/// an id in the default catalogue of some other provider this build knows.
/// That is the case ADR-0031 guards against, a card set to one provider's
/// model reaching another provider's runner. A model no provider claims runs:
/// `tasks.model` is free text, and Claude runs a full id (`claude-…`) that no
/// catalogue lists, so a narrower reading would skip tomorrow a card that
/// runs tonight.
///
/// **A known limitation.** `strategy_catalogue` is one stored document per
/// team, not one per provider, and [`resolve`] fills it from the provider's
/// defaults field by field. Once a team edits `models` to list Claude's
/// models, every provider's resolved catalogue lists them, so this answers
/// `true` for a Ledger runner too, and one Claude-oriented edit turns the
/// rule off for every other provider's runners. That is accepted while no
/// second production provider exists. The follow-up is a catalogue per
/// provider, which needs an ADR-0028 amendment, because ADR-0028 places
/// `strategy_catalogue` as one team setting (seam-contract D31, "What task
/// 067 decided"). A unit case pins today's behaviour so that change is made
/// deliberately.
pub fn runs_on(model: &str, provider: ProviderId, resolved: &Catalogue) -> bool {
    let lists = |catalogue: &Catalogue| catalogue.models.iter().any(|entry| entry.id == model);
    lists(resolved)
        || !ProviderId::ALL
            .iter()
            .filter(|other| **other != provider)
            .any(|other| lists(&other.default_catalogue()))
}

/// The context's one team's catalogue as the text the operator typed, or
/// `None` when they never wrote one: what Settings opens its textarea on.
pub async fn stored_text(ctx: &ServiceContext) -> Result<Option<String>> {
    settings::get_team(ctx, ctx.scope.sole()?, STRATEGY_CATALOGUE).await
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
    parse_raw(trimmed)
        .map_err(|message| Error::invalid(format!("the catalogue is not valid JSON: {message}")))?;

    settings::set_team(ctx, ctx.scope.sole()?, STRATEGY_CATALOGUE, Some(trimmed)).await
}

/// The catalogue as the operator wrote it — every field `Option`, so an absent
/// one and an explicitly empty one stay told apart until [`resolve`] decides
/// what each means. `deny_unknown_fields` for the reason [`Catalogue`]'s own
/// doc gives.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalogue {
    models: Option<Vec<CatalogueEntry>>,
    efforts: Option<Vec<CatalogueEntry>>,
    planner: Option<PlannerBudget>,
}

/// One parser, so the tolerant read and the refusing write cannot disagree
/// about what is valid.
fn parse_raw(stored: &str) -> std::result::Result<RawCatalogue, String> {
    serde_json::from_str(stored).map_err(|error| error.to_string())
}

/// A field the operator did not write reaches for `provider`'s own default; a
/// field they wrote — even as an empty list — stays exactly what they wrote.
/// "`\"models\": []`" is an operator saying "offer nothing", which is a thing
/// they are allowed to do, and silently restoring the default would be the
/// same defect [`crate::db::settings::base_instructions`] documents about a
/// field a user cleared.
fn resolve(raw: RawCatalogue, default: Catalogue) -> Catalogue {
    Catalogue {
        models: raw.models.unwrap_or(default.models),
        efforts: raw.efforts.unwrap_or(default.efforts),
        planner: raw.planner.unwrap_or(default.planner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::provider::{AgentProvider, ClaudeProvider};
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;

    fn entries(pairs: &[(&str, &str)]) -> Vec<CatalogueEntry> {
        pairs
            .iter()
            .map(|(id, label)| CatalogueEntry {
                id: (*id).to_string(),
                label: (*label).to_string(),
            })
            .collect()
    }

    #[tokio::test]
    async fn an_unconfigured_catalogue_is_the_providers_own_default() {
        let h = TestContext::new().await;

        assert_eq!(
            settings::get_team(&h.context, &h.solo.team_id, STRATEGY_CATALOGUE)
                .await
                .expect("read the key"),
            None,
            "the key is deliberately unseeded"
        );
        assert_eq!(
            catalogue(&h.context, &ClaudeProvider)
                .await
                .expect("read the default"),
            ClaudeProvider.default_catalogue()
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
            settings::set_team(&h.context, &h.solo.team_id, STRATEGY_CATALOGUE, Some(typo))
                .await
                .expect("store a typo");

            assert_eq!(
                catalogue(&h.context, &ClaudeProvider)
                    .await
                    .expect("read it back"),
                ClaudeProvider.default_catalogue(),
                "a hand-edited {typo:?} must cost a log line, not a launch"
            );
        }
    }

    #[tokio::test]
    async fn an_explicitly_empty_model_list_means_no_choices_not_the_default_list() {
        // The operator turning a dropdown off is a thing they are allowed to
        // do — `board::service::team_disallowed_tools`' established rule. Note that
        // `efforts`, which they did *not* write, still fills in from the
        // provider.
        let h = TestContext::new().await;

        settings::set_team(
            &h.context,
            &h.solo.team_id,
            STRATEGY_CATALOGUE,
            Some(r#"{"models": []}"#),
        )
        .await
        .expect("store an empty model list");

        let stored = catalogue(&h.context, &ClaudeProvider)
            .await
            .expect("read it back");

        assert_eq!(stored.models, Vec::new());
        assert_eq!(stored.efforts, ClaudeProvider.default_catalogue().efforts);
        assert_eq!(stored.planner, ClaudeProvider.default_catalogue().planner);
    }

    #[tokio::test]
    async fn a_planner_with_no_model_passes_no_model_flag_at_all() {
        // The last row of task 020's absent-value table: an explicit planner
        // object without a `model` means the CLI chooses, not the provider's
        // own pick.
        let h = TestContext::new().await;

        settings::set_team(
            &h.context,
            &h.solo.team_id,
            STRATEGY_CATALOGUE,
            Some(r#"{"planner": {}}"#),
        )
        .await
        .expect("store a planner with no model");

        let planner = catalogue(&h.context, &ClaudeProvider)
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
    fn an_unedited_catalogue_and_planner_budget_carry_no_providers_opinion() {
        // `Catalogue::default` and `PlannerBudget::default` are the plain Rust
        // values with nobody's model names in them — what an *installation*
        // offers unconfigured is `AgentProvider::default_catalogue`, asserted
        // against `ClaudeProvider` in `runner::provider::claude`'s own tests.
        assert_eq!(Catalogue::default().models, Vec::<CatalogueEntry>::new());
        assert_eq!(Catalogue::default().efforts, Vec::<CatalogueEntry>::new());
        assert_eq!(PlannerBudget::default().model, None);
        assert_eq!(PlannerBudget::default().effort, None);
        assert_eq!(
            PlannerBudget::default().max_turns,
            DEFAULT_PLANNER_MAX_TURNS
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
            settings::get_team(&h.context, &h.solo.team_id, STRATEGY_CATALOGUE)
                .await
                .expect("read the row"),
            Some(edited.to_string()),
            "the user's own formatting is what Settings shows them next time"
        );
        assert_eq!(
            catalogue(&h.context, &ClaudeProvider)
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
            settings::get_team(&h.context, &h.solo.team_id, STRATEGY_CATALOGUE)
                .await
                .expect("read the key"),
            None,
            "a refused write stores nothing"
        );
    }

    // Task 067's model rule. Ledger's defaults are `steady` and `swift`,
    // Claude's `opus`, `sonnet` and `haiku`.

    #[test]
    fn a_model_in_the_runners_catalogue_runs() {
        assert!(runs_on(
            "steady",
            ProviderId::Ledger,
            &ProviderId::Ledger.default_catalogue()
        ));
        assert!(runs_on(
            "opus",
            ProviderId::ClaudeCode,
            &ProviderId::ClaudeCode.default_catalogue()
        ));
    }

    #[test]
    fn another_providers_default_model_does_not_run() {
        assert!(!runs_on(
            "opus",
            ProviderId::Ledger,
            &ProviderId::Ledger.default_catalogue()
        ));
        assert!(!runs_on(
            "swift",
            ProviderId::ClaudeCode,
            &ProviderId::ClaudeCode.default_catalogue()
        ));
    }

    #[test]
    fn a_model_no_provider_lists_runs() {
        // A full model id reaches the CLI as it always has: nothing claims it,
        // so nothing says it belongs to another provider.
        for provider in ProviderId::ALL.iter().copied() {
            assert!(
                runs_on("claude-opus-4-5", provider, &provider.default_catalogue()),
                "{provider}"
            );
        }
    }

    #[test]
    fn an_edited_catalogue_that_lists_another_providers_model_runs() {
        let edited = Catalogue {
            models: entries(&[("opus", "Opus"), ("swift", "Swift")]),
            ..ProviderId::ClaudeCode.default_catalogue()
        };

        assert!(runs_on("swift", ProviderId::ClaudeCode, &edited));
    }

    #[tokio::test]
    async fn an_edited_catalogue_listing_claudes_models_lets_a_ledger_runner_take_them() {
        // The known limitation, pinned: one stored document per team, filled
        // from each provider's defaults, so a Claude-oriented edit lists
        // Claude's models for a Ledger runner too and the rule stops firing.
        // A catalogue per provider (an ADR-0028 amendment) changes this
        // answer, and should change this test with it.
        let h = TestContext::new().await;
        settings::set_team(
            &h.context,
            &h.solo.team_id,
            STRATEGY_CATALOGUE,
            Some(r#"{"models": [{"id": "opus", "label": "Opus"}]}"#),
        )
        .await
        .expect("store a Claude-oriented edit");

        let for_ledger = catalogue_for_provider(&h.context, &h.solo.team_id, ProviderId::Ledger)
            .await
            .expect("resolve the catalogue for Ledger");

        assert_eq!(for_ledger.models, entries(&[("opus", "Opus")]));
        assert!(runs_on("opus", ProviderId::Ledger, &for_ledger));
    }

    #[tokio::test]
    async fn writing_the_catalogue_publishes_settings() {
        // Settings re-reads on `settings:changed`, which is how a second window
        // learns a model was added (ADR-0018).
        let mut h = TestContext::new().await;

        set_catalogue(
            &h.context,
            r#"{"models": [{"id": "opus", "label": "Opus"}]}"#,
        )
        .await
        .expect("store an edit");

        assert_eq!(
            h.changes.try_recv().expect("a publication"),
            crate::ChangeEvent::settings(h.solo.team_id.clone())
        );
    }
}
