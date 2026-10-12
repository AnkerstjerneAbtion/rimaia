//! The strategy defaults and the approval flag (ADR-0016, seam-contract
//! D17.2).
//!
//! Four keys, and none of them is a column. Everything task 020 stores beyond
//! the six `tasks` columns the initial schema already carries is
//! *configuration*, and `settings` is the configuration table (D3), which is
//! how D4's count of three migrations stays at three:
//!
//! ```text
//! strategy_catalogue                model and effort lists, and the planner's budget
//! strategy_default                  global StrategyDefaults JSON
//! strategy_default.<repository_id>  per-repository StrategyDefaults JSON
//! strategy_approval                 "automatic" | "manual"
//! ```
//!
//! The first is [`super::catalogue`]'s; the other three are here. Storage is
//! task 006's accessor in every case — what a key means, and what an absent one
//! means, is what lives with the module (D3, repeated by D16.2).
//!
//! **The named cost of keying per-repository defaults instead of adding a
//! column:** a settings key is not a foreign key and nothing cascades, so
//! [`crate::repo::remove`] deletes that repository's row explicitly. D17.1 says
//! so out loud precisely so that nobody meets it later as a bug report about
//! orphan rows.
//!
//! `strategy_approval` is stored and rendered by task 020 and **read by
//! nothing** — the approval gate is deferred until after tasks 011 and 012 so
//! that it does not contend with their `selection.rs` restructure. It is here
//! rather than in that later task because the Settings panel ships now, and a
//! radio group that forgets its answer on relaunch is worse than no radio group.

use serde::{Deserialize, Serialize};

use crate::context::ServiceContext;
use crate::db::{settings, StrategyMode};
use crate::error::{Error, Result};

/// The `settings` key holding the global defaults. Also the prefix every
/// per-repository key is built from — see [`repository_default_key`].
pub const STRATEGY_DEFAULT: &str = "strategy_default";

/// The `settings` key holding whether a proposal needs a human before the run
/// starts.
pub const STRATEGY_APPROVAL: &str = "strategy_approval";

/// The key holding `repository_id`'s own defaults.
///
/// A function rather than a `format!` at each call site, because the shape of
/// this key is the only thing standing between a stored default and the row
/// [`crate::repo::remove`] has to delete: two spellings of it would leak a row
/// per removed repository and nothing would ever notice.
pub fn repository_default_key(repository_id: &str) -> String {
    format!("{STRATEGY_DEFAULT}.{repository_id}")
}

/// A default strategy — global, or one repository's.
///
/// The same struct at both levels on purpose. They are read by one function,
/// parsed by one parser, and combined by one precedence chain
/// ([`super::resolve`]); a per-repository shape that differed from the global
/// one would be two rules where the product has one.
///
/// [`StrategyMode::Default`] is how this says "no opinion". The mode enum has
/// no `inherit` variant — the column it mirrors is `NOT NULL DEFAULT 'default'`
/// — so `Default` means *fall through* here for exactly the reason D17.6 gives
/// it that meaning on a task.
/// `JsonSchema` because ADR-0021 puts this on the tool surface, and because
/// unlike a row type it *is* the wire shape: seam-contract D16.1 keeps row types
/// out of `mcp::responses` by projecting them, but a catalogue is a
/// configuration document whose serde shape is already what gets stored and what
/// the operator edits. A projection here would be a second spelling of one
/// document, free to drift from the thing it describes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StrategyDefaults {
    pub mode: StrategyMode,
    /// Free text, not an enum, for the reason [`crate::db::Task::model`] gives:
    /// a closed set here is a release blocker the first time Anthropic names
    /// something new.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl Default for StrategyDefaults {
    fn default() -> Self {
        Self {
            mode: StrategyMode::Default,
            model: None,
            effort: None,
        }
    }
}

/// Whether a planner's proposal runs on its own or waits for a human.
///
/// Two values, and [`Automatic`](StrategyApproval::Automatic) is the default an
/// absent key stands for — an overnight queue that stops to ask is the thing
/// ADR-0016's "can let the queue proceed without waiting for approval" exists
/// to avoid, and the queue is the reason this product exists.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StrategyApproval {
    /// The proposal is applied and the implementation run follows it.
    #[default]
    Automatic,
    /// The proposal waits on the card until a human accepts it. **Stored and
    /// rendered by task 020, read by nothing yet** — see the module docs.
    Manual,
}

impl StrategyApproval {
    /// The stored spelling, which is also the wire spelling — one string, so
    /// the row stays legible in the `sqlite3` CLI (ADR-0003).
    pub const fn as_str(self) -> &'static str {
        match self {
            StrategyApproval::Automatic => "automatic",
            StrategyApproval::Manual => "manual",
        }
    }

    /// Reads a stored value, falling back to the default for anything else —
    /// the tolerance rule
    /// [`RunEnvironment::from_stored`](crate::db::RunEnvironment) states, for
    /// the same reason: `settings.value` has no `CHECK` and the user is a
    /// supported writer of this file (ADR-0003).
    fn from_stored(value: &str) -> Self {
        match value {
            "automatic" => StrategyApproval::Automatic,
            "manual" => StrategyApproval::Manual,
            other => {
                tracing::warn!(
                    value = other,
                    "unrecognised strategy_approval; falling back to automatic"
                );
                StrategyApproval::default()
            }
        }
    }
}

/// The context's one team's defaults: what applies when neither a task nor
/// its repository says anything, as Settings shows and edits it.
pub async fn global_default(ctx: &ServiceContext) -> Result<StrategyDefaults> {
    global_default_for(ctx, ctx.scope.sole()?).await
}

/// One team's defaults, for a task of that team whichever teams the context
/// reaches.
pub async fn global_default_for(ctx: &ServiceContext, team_id: &str) -> Result<StrategyDefaults> {
    defaults_at(ctx, team_id, STRATEGY_DEFAULT).await
}

/// The defaults `repository_id` sets for every task in it — ADR-0016's "a repo
/// of small tasks can default low without touching each card".
///
/// The repository is looked up first, in the context's scope, so another
/// team's repository is `NotFound` exactly as one that was never registered,
/// and its key is read from the repository's own team.
pub async fn repository_default(
    ctx: &ServiceContext,
    repository_id: &str,
) -> Result<StrategyDefaults> {
    let team_id = crate::repo::team_of(ctx, repository_id).await?;
    defaults_at(ctx, &team_id, &repository_default_key(repository_id)).await
}

pub async fn set_global_default(ctx: &ServiceContext, value: &StrategyDefaults) -> Result<()> {
    let team_id = ctx.scope.sole()?.clone();
    store_defaults(ctx, &team_id, STRATEGY_DEFAULT, value).await
}

/// Stores `repository_id`'s defaults under the repository's own team, after
/// the same scoped lookup [`repository_default`] makes.
pub async fn set_repository_default(
    ctx: &ServiceContext,
    repository_id: &str,
    value: &StrategyDefaults,
) -> Result<()> {
    let team_id = crate::repo::team_of(ctx, repository_id).await?;
    store_defaults(ctx, &team_id, &repository_default_key(repository_id), value).await
}

/// How much of a proposal a human has to look at before it runs, for the
/// context's one team. Absent means [`StrategyApproval::Automatic`].
pub async fn approval(ctx: &ServiceContext) -> Result<StrategyApproval> {
    Ok(
        settings::get_team(ctx, ctx.scope.sole()?, STRATEGY_APPROVAL)
            .await?
            .as_deref()
            .map(StrategyApproval::from_stored)
            .unwrap_or_default(),
    )
}

pub async fn set_approval(ctx: &ServiceContext, value: StrategyApproval) -> Result<()> {
    settings::set_team(
        ctx,
        ctx.scope.sole()?,
        STRATEGY_APPROVAL,
        Some(value.as_str()),
    )
    .await
}

/// One reader and one absent-value rule for both levels of default.
///
/// The global key and a per-repository key differ only in their name, so they
/// differ only here. That is the whole of D3's argument applied one level down:
/// a second parser for the per-repository case is a second place for
/// `"planned"` to stop meaning planned.
async fn defaults_at(ctx: &ServiceContext, team_id: &str, key: &str) -> Result<StrategyDefaults> {
    let Some(stored) = settings::get_team(ctx, team_id, key).await? else {
        return Ok(StrategyDefaults::default());
    };

    Ok(serde_json::from_str(&stored).unwrap_or_else(|error| {
        tracing::warn!(
            key,
            error = error.to_string(),
            "unparseable strategy default; falling back to no opinion"
        );
        StrategyDefaults::default()
    }))
}

/// One writer, refusing what [`defaults_at`] would have to warn about.
///
/// Serialized here rather than accepting text, because unlike the catalogue
/// these are three form controls and never a textarea — there is no user
/// formatting to preserve, and no way for the value to be invalid by the time
/// it reaches this function.
async fn store_defaults(
    ctx: &ServiceContext,
    team_id: &str,
    key: &str,
    value: &StrategyDefaults,
) -> Result<()> {
    let json = serde_json::to_string(value).map_err(|error| {
        Error::internal(format!("the strategy default did not serialize: {error}"))
    })?;

    settings::set_team(ctx, team_id, key, Some(&json)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Repository;
    use crate::testing::{TempRepo, TestContext};
    use crate::{repo, ChangeEvent};
    use pretty_assertions::assert_eq;

    /// A real repository, because a repository default is read and written
    /// only after the repository is looked up in the context's scope. The
    /// source and worktrees directories live as long as the test does.
    struct Registered {
        repository: Repository,
        _source: TempRepo,
        _worktrees: tempfile::TempDir,
    }

    async fn registered(h: &TestContext) -> Registered {
        let source = TempRepo::init();
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("a worktrees directory");
        let repository = repo::register(
            &h.context,
            h.machine(),
            worktrees.path(),
            repo::NewRepository {
                path: source.path().to_str().expect("a UTF-8 path").to_string(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a real repository");
        Registered {
            repository,
            _source: source,
            _worktrees: worktrees,
        }
    }

    #[tokio::test]
    async fn a_repository_default_is_stored_under_its_own_key_and_read_back() {
        let h = TestContext::new().await;
        let registered = registered(&h).await;
        let repository_id = registered.repository.id.clone();

        set_repository_default(
            &h.context,
            &repository_id,
            &StrategyDefaults {
                mode: StrategyMode::Manual,
                model: Some("haiku".to_string()),
                effort: Some("low".to_string()),
            },
        )
        .await
        .expect("store a repository default");

        assert_eq!(
            settings::get_team(
                &h.context,
                &h.solo.team_id,
                &format!("strategy_default.{repository_id}")
            )
            .await
            .expect("read the row"),
            Some(r#"{"mode":"manual","model":"haiku","effort":"low"}"#.to_string()),
            "the key names the repository, and the value stays legible in the sqlite3 CLI"
        );
        assert_eq!(
            repository_default(&h.context, &repository_id)
                .await
                .expect("read it back"),
            StrategyDefaults {
                mode: StrategyMode::Manual,
                model: Some("haiku".to_string()),
                effort: Some("low".to_string()),
            }
        );
        assert_eq!(
            global_default(&h.context)
                .await
                .expect("read the global default"),
            StrategyDefaults::default(),
            "one repository's opinion is not everyone's"
        );
    }

    #[tokio::test]
    async fn a_repository_default_for_an_unregistered_repository_is_not_found() {
        // The repository is looked up first, so an id the scope does not hold
        // is refused rather than stored under a key nothing will ever read.
        let h = TestContext::new().await;
        let missing = "3f2b1c00-0000-4000-8000-000000000002";

        for error in [
            repository_default(&h.context, missing)
                .await
                .expect_err("no such repository"),
            set_repository_default(&h.context, missing, &StrategyDefaults::default())
                .await
                .expect_err("no such repository"),
        ] {
            assert_eq!(error.code(), crate::ErrorCode::NotFound);
            assert_eq!(
                error.to_string(),
                format!("no repository with id {missing}")
            );
        }
    }

    #[tokio::test]
    async fn removing_a_repository_removes_its_strategy_default_row() {
        // A settings key is not a foreign key and nothing cascades (D17.1).
        // Real git, because `repo::register` validates a real repository.
        let h = TestContext::new().await;
        let registered = registered(&h).await;
        let repository = &registered.repository;

        set_repository_default(
            &h.context,
            &repository.id,
            &StrategyDefaults {
                mode: StrategyMode::Planned,
                ..Default::default()
            },
        )
        .await
        .expect("store a repository default");

        repo::remove(&h.context, Some(h.machine()), &repository.id)
            .await
            .expect("removal with no referencing tasks must succeed");

        assert_eq!(
            settings::get_team(
                &h.context,
                &h.solo.team_id,
                &repository_default_key(&repository.id)
            )
            .await
            .expect("look for the row"),
            None,
            "the orphan row has to go with the repository that owned it"
        );
    }

    #[tokio::test]
    async fn removing_a_repository_announces_only_the_repository_change() {
        // The default leaves inside the removal's own transaction, so the
        // removal is one write and one announcement, as it was in solo before
        // team settings had a writer of their own (039's Goal).
        let mut h = TestContext::new().await;
        let registered = registered(&h).await;
        let repository = &registered.repository;
        set_repository_default(
            &h.context,
            &repository.id,
            &StrategyDefaults {
                mode: StrategyMode::Planned,
                ..Default::default()
            },
        )
        .await
        .expect("store a repository default");
        while h.changes.try_recv().is_ok() {}

        // The board's removal alone: given a machine, forgetting the checkout
        // announces the same repository again, which is the machine's event
        // and not this one's subject.
        repo::remove(&h.context, None, &repository.id)
            .await
            .expect("removal with no referencing tasks must succeed");

        let mut published = Vec::new();
        while let Ok(event) = h.changes.try_recv() {
            published.push(event);
        }
        assert_eq!(
            published,
            vec![ChangeEvent::repositories(
                h.solo.team_id.clone(),
                [repository.id.clone()]
            )]
        );
    }

    #[tokio::test]
    async fn a_refused_repository_removal_keeps_its_strategy_default() {
        // Why the default is removed in the repository's own transaction: a
        // refusal rolls both back, and a repository that is still referenced
        // is still configured.
        let mut h = TestContext::new().await;
        let registered = registered(&h).await;
        let repository = &registered.repository;

        set_repository_default(
            &h.context,
            &repository.id,
            &StrategyDefaults {
                mode: StrategyMode::Planned,
                ..Default::default()
            },
        )
        .await
        .expect("store a repository default");

        const NOW: &str = "2026-08-20T12:00:00+00:00";
        sqlx::query!(
            "INSERT INTO tasks (id, team_id, repository_id, title, board_column, position, run_state, created_at, updated_at)
             VALUES ('3f2b1c00-0000-4000-8000-00000000000a', ?3, ?1, 'Still here', 'ready', 1.0, 'idle', ?2, ?2)",
            repository.id,
            NOW,
            h.solo.team_id,
        )
        .execute(&h.context.pool)
        .await
        .expect("insert a referencing task");
        while h.changes.try_recv().is_ok() {}

        repo::remove(&h.context, Some(h.machine()), &repository.id)
            .await
            .expect_err("removal must be refused while a task references it");

        assert!(
            h.changes.try_recv().is_err(),
            "a refused removal changed nothing, so it announces nothing"
        );
        assert_eq!(
            repository_default(&h.context, &repository.id)
                .await
                .expect("read it back"),
            StrategyDefaults {
                mode: StrategyMode::Planned,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn an_absent_approval_setting_is_automatic() {
        let h = TestContext::new().await;

        assert_eq!(
            settings::get_team(&h.context, &h.solo.team_id, STRATEGY_APPROVAL)
                .await
                .expect("read the key"),
            None,
            "the key is deliberately unseeded"
        );
        assert_eq!(
            approval(&h.context).await.expect("read the default"),
            StrategyApproval::Automatic
        );
    }

    #[tokio::test]
    async fn a_stored_approval_round_trips_through_its_spelling() {
        let h = TestContext::new().await;

        set_approval(&h.context, StrategyApproval::Manual)
            .await
            .expect("store manual approval");

        assert_eq!(
            settings::get_team(&h.context, &h.solo.team_id, STRATEGY_APPROVAL)
                .await
                .expect("read the row"),
            Some("manual".to_string())
        );
        assert_eq!(
            approval(&h.context).await.expect("read it back"),
            StrategyApproval::Manual
        );
    }

    #[tokio::test]
    async fn a_hand_edited_approval_falls_back_to_automatic_instead_of_failing() {
        let h = TestContext::new().await;

        settings::set_team(
            &h.context,
            &h.solo.team_id,
            STRATEGY_APPROVAL,
            Some("ask me"),
        )
        .await
        .expect("store a typo");

        assert_eq!(
            approval(&h.context).await.expect("read it back"),
            StrategyApproval::Automatic
        );
    }

    #[tokio::test]
    async fn an_absent_default_is_no_opinion_at_either_level() {
        let h = TestContext::new().await;
        let registered = registered(&h).await;

        assert_eq!(
            global_default(&h.context)
                .await
                .expect("read the global default"),
            StrategyDefaults::default()
        );
        assert_eq!(
            repository_default(&h.context, &registered.repository.id)
                .await
                .expect("read a repository default"),
            StrategyDefaults::default()
        );
        assert_eq!(StrategyDefaults::default().mode, StrategyMode::Default);
    }

    #[tokio::test]
    async fn a_hand_edited_default_falls_back_to_no_opinion_at_either_level() {
        // One parser means one tolerance rule, so this asserts both keys rather
        // than trusting that the second call site copied the first.
        let h = TestContext::new().await;
        let repository_key = repository_default_key("3f2b1c00-0000-4000-8000-000000000002");

        for key in [STRATEGY_DEFAULT, repository_key.as_str()] {
            for typo in ["", "{", r#"{"mode":"planed"}"#, r#"{"mdel":"opus"}"#] {
                settings::set_team(&h.context, &h.solo.team_id, key, Some(typo))
                    .await
                    .expect("store a typo");

                assert_eq!(
                    defaults_at(&h.context, &h.solo.team_id, key)
                        .await
                        .expect("read it back"),
                    StrategyDefaults::default(),
                    "a hand-edited {typo:?} under {key} must cost a log line, not a launch"
                );
            }
        }
    }

    #[tokio::test]
    async fn a_default_with_only_a_model_leaves_the_mode_alone() {
        let h = TestContext::new().await;

        settings::set_team(
            &h.context,
            &h.solo.team_id,
            STRATEGY_DEFAULT,
            Some(r#"{"model":"sonnet"}"#),
        )
        .await
        .expect("store a partial default");

        assert_eq!(
            global_default(&h.context).await.expect("read it back"),
            StrategyDefaults {
                mode: StrategyMode::Default,
                model: Some("sonnet".to_string()),
                effort: None,
            },
            "a repository that only pins a model has not also asked for manual mode"
        );
    }

    #[tokio::test]
    async fn writing_a_default_publishes_settings() {
        let mut h = TestContext::new().await;

        set_global_default(&h.context, &StrategyDefaults::default())
            .await
            .expect("store the global default");

        assert_eq!(
            h.changes.try_recv().expect("a publication"),
            ChangeEvent::settings(h.solo.team_id.clone())
        );
    }
}
