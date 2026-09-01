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
use sqlx::SqlitePool;

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

/// The defaults that apply when neither the task nor its repository says
/// anything.
pub async fn global_default(pool: &SqlitePool) -> Result<StrategyDefaults> {
    defaults_at(pool, STRATEGY_DEFAULT).await
}

/// The defaults `repository_id` sets for every task in it — ADR-0016's "a repo
/// of small tasks can default low without touching each card".
pub async fn repository_default(
    pool: &SqlitePool,
    repository_id: &str,
) -> Result<StrategyDefaults> {
    defaults_at(pool, &repository_default_key(repository_id)).await
}

pub async fn set_global_default(ctx: &ServiceContext, value: &StrategyDefaults) -> Result<()> {
    store_defaults(ctx, STRATEGY_DEFAULT, value).await
}

pub async fn set_repository_default(
    ctx: &ServiceContext,
    repository_id: &str,
    value: &StrategyDefaults,
) -> Result<()> {
    store_defaults(ctx, &repository_default_key(repository_id), value).await
}

/// Removes `repository_id`'s defaults, leaving no orphan row behind (D17.1).
///
/// Takes an executor rather than a [`ServiceContext`] so that
/// [`crate::repo::remove`] can run it inside the transaction that deletes the
/// repository itself: a removal refused because tasks still reference it must
/// not have thrown the repository's configuration away on the way to the
/// refusal. Nothing is published either — the caller already announces
/// [`ChangeEvent::repositories`](crate::ChangeEvent::repositories), and the
/// only surface that renders this row is the repository row that just
/// disappeared.
pub async fn delete_repository_default<'e, E>(executor: E, repository_id: &str) -> Result<()>
where
    E: sqlx::SqliteExecutor<'e>,
{
    let key = repository_default_key(repository_id);
    sqlx::query!("DELETE FROM settings WHERE key = ?1", key)
        .execute(executor)
        .await?;
    Ok(())
}

/// How much of a proposal a human has to look at before it runs. Absent means
/// [`StrategyApproval::Automatic`].
pub async fn approval(pool: &SqlitePool) -> Result<StrategyApproval> {
    Ok(settings::get(pool, STRATEGY_APPROVAL)
        .await?
        .as_deref()
        .map(StrategyApproval::from_stored)
        .unwrap_or_default())
}

pub async fn set_approval(ctx: &ServiceContext, value: StrategyApproval) -> Result<()> {
    settings::set(ctx, STRATEGY_APPROVAL, value.as_str()).await
}

/// One reader and one absent-value rule for both levels of default.
///
/// The global key and a per-repository key differ only in their name, so they
/// differ only here. That is the whole of D3's argument applied one level down:
/// a second parser for the per-repository case is a second place for
/// `"planned"` to stop meaning planned.
async fn defaults_at(pool: &SqlitePool, key: &str) -> Result<StrategyDefaults> {
    let Some(stored) = settings::get(pool, key).await? else {
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
async fn store_defaults(ctx: &ServiceContext, key: &str, value: &StrategyDefaults) -> Result<()> {
    let json = serde_json::to_string(value).map_err(|error| {
        Error::internal(format!("the strategy default did not serialize: {error}"))
    })?;

    settings::set(ctx, key, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TempRepo, TestContext};
    use crate::{repo, ChangeEvent};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn a_repository_default_is_stored_under_its_own_key_and_read_back() {
        let h = TestContext::new().await;

        set_repository_default(
            &h.context,
            "3f2b1c00-0000-4000-8000-000000000002",
            &StrategyDefaults {
                mode: StrategyMode::Manual,
                model: Some("haiku".to_string()),
                effort: Some("low".to_string()),
            },
        )
        .await
        .expect("store a repository default");

        assert_eq!(
            settings::get(
                &h.context.pool,
                "strategy_default.3f2b1c00-0000-4000-8000-000000000002"
            )
            .await
            .expect("read the row"),
            Some(r#"{"mode":"manual","model":"haiku","effort":"low"}"#.to_string()),
            "the key names the repository, and the value stays legible in the sqlite3 CLI"
        );
        assert_eq!(
            repository_default(&h.context.pool, "3f2b1c00-0000-4000-8000-000000000002")
                .await
                .expect("read it back"),
            StrategyDefaults {
                mode: StrategyMode::Manual,
                model: Some("haiku".to_string()),
                effort: Some("low".to_string()),
            }
        );
        assert_eq!(
            global_default(&h.context.pool)
                .await
                .expect("read the global default"),
            StrategyDefaults::default(),
            "one repository's opinion is not everyone's"
        );
    }

    #[tokio::test]
    async fn removing_a_repository_removes_its_strategy_default_row() {
        // A settings key is not a foreign key and nothing cascades (D17.1).
        // Real git, because `repo::register` validates a real repository.
        let h = TestContext::new().await;
        let source = TempRepo::init();
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("a worktrees directory");
        let repository = repo::register(
            &h.context,
            worktrees.path(),
            repo::NewRepository {
                path: source.path().to_str().expect("a UTF-8 path").to_string(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a real repository");

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

        repo::remove(&h.context, &repository.id)
            .await
            .expect("removal with no referencing tasks must succeed");

        assert_eq!(
            settings::get(&h.context.pool, &repository_default_key(&repository.id))
                .await
                .expect("look for the row"),
            None,
            "the orphan row has to go with the repository that owned it"
        );
    }

    #[tokio::test]
    async fn a_refused_repository_removal_keeps_its_strategy_default() {
        // The reason the delete runs inside `remove`'s transaction rather than
        // after it: a repository that is still referenced is still configured.
        let h = TestContext::new().await;
        let source = TempRepo::init();
        let worktrees = tempfile::Builder::new()
            .prefix("rimaia-worktrees-")
            .tempdir()
            .expect("a worktrees directory");
        let repository = repo::register(
            &h.context,
            worktrees.path(),
            repo::NewRepository {
                path: source.path().to_str().expect("a UTF-8 path").to_string(),
                name: None,
                worktree_root: None,
            },
        )
        .await
        .expect("register a real repository");

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
            "INSERT INTO tasks (id, repository_id, title, board_column, position, run_state, created_at, updated_at)
             VALUES ('3f2b1c00-0000-4000-8000-00000000000a', ?1, 'Still here', 'ready', 1.0, 'idle', ?2, ?2)",
            repository.id,
            NOW,
        )
        .execute(&h.context.pool)
        .await
        .expect("insert a referencing task");

        repo::remove(&h.context, &repository.id)
            .await
            .expect_err("removal must be refused while a task references it");

        assert_eq!(
            repository_default(&h.context.pool, &repository.id)
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
        let pool = test_pool().await;

        assert_eq!(
            settings::get(&pool, STRATEGY_APPROVAL)
                .await
                .expect("read the key"),
            None,
            "the key is deliberately unseeded"
        );
        assert_eq!(
            approval(&pool).await.expect("read the default"),
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
            settings::get(&h.context.pool, STRATEGY_APPROVAL)
                .await
                .expect("read the row"),
            Some("manual".to_string())
        );
        assert_eq!(
            approval(&h.context.pool).await.expect("read it back"),
            StrategyApproval::Manual
        );
    }

    #[tokio::test]
    async fn a_hand_edited_approval_falls_back_to_automatic_instead_of_failing() {
        let h = TestContext::new().await;

        settings::set(&h.context, STRATEGY_APPROVAL, "ask me")
            .await
            .expect("store a typo");

        assert_eq!(
            approval(&h.context.pool).await.expect("read it back"),
            StrategyApproval::Automatic
        );
    }

    #[tokio::test]
    async fn an_absent_default_is_no_opinion_at_either_level() {
        let pool = test_pool().await;

        assert_eq!(
            global_default(&pool)
                .await
                .expect("read the global default"),
            StrategyDefaults::default()
        );
        assert_eq!(
            repository_default(&pool, "3f2b1c00-0000-4000-8000-000000000002")
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
                settings::set(&h.context, key, typo)
                    .await
                    .expect("store a typo");

                assert_eq!(
                    defaults_at(&h.context.pool, key)
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

        settings::set(&h.context, STRATEGY_DEFAULT, r#"{"model":"sonnet"}"#)
            .await
            .expect("store a partial default");

        assert_eq!(
            global_default(&h.context.pool).await.expect("read it back"),
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
            ChangeEvent::Settings
        );
    }
}
