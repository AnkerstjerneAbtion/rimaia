//! The typed view of the `settings` key/value table (seam-contract D3).
//!
//! [`Setting`](crate::db::Setting) is the row and nothing more. *Which* keys
//! exist, what each one holds, and what an absent key means are business rules,
//! and they live here so that every reader gets the same answer — task 008 in
//! particular reads [`run_environment`] through this module rather than through
//! SQL of its own, so `inherit | strict_local` is parsed in one place instead of
//! two.
//!
//! Writes publish [`ChangeEvent::Settings`] after the row is committed
//! (ADR-0018). The event carries no key: the whole table is a handful of rows
//! and every consumer re-reads all of it.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::doctor::Check;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;

/// The global instructions prepended to every composed run prompt (ADR-0009).
///
/// Seeded by `src-tauri/migrations/20260820120100_seed_settings.sql`.
pub const BASE_INSTRUCTIONS: &str = "base_instructions";

/// Whether a run inherits the operator's Claude Code configuration
/// (ADR-0004's amendment). Deliberately unseeded — see [`RunEnvironment`].
pub const RUN_ENVIRONMENT: &str = "run_environment";

/// Task 018's first-run walkthrough, seen or skipped. See
/// [`onboarding_dismissed`] for why this is a key rather than only a derivation.
pub const ONBOARDING_DISMISSED: &str = "onboarding_dismissed";

/// Task 024's subscription figure — what the user says they pay per month.
///
/// **Absent is not zero.** Absent means the comparison is not rendered at all;
/// a zero would be a claim that the subscription is free.
pub const SUBSCRIPTION_MONTHLY_USD: &str = "subscription_monthly_usd";

/// Task 027's dismissed doctor warnings — a JSON array of [`Dismissal`].
///
/// A settings key rather than a table for seam-contract D4's reason: the
/// migration list is closed, and a set of strings only the doctor reads is what
/// the key/value table is for.
pub const DOCTOR_DISMISSALS: &str = "doctor_dismissals";

/// What the migration writes into [`BASE_INSTRUCTIONS`] on first launch.
///
/// Exported so a future "restore the default" action in Settings has a value
/// to write back — no such control exists yet; task 006's Scope does not ask
/// for one.
///
/// Duplicated between this constant and the migration on purpose: the migration
/// is the seed and cannot call Rust, and a test in this module pins the two
/// together byte for byte so the pair cannot drift.
pub const DEFAULT_BASE_INSTRUCTIONS: &str = "\
Commit as you work, with focused commits and clear messages.
Run the project's tests and linters before you finish.
When the work is complete, push the branch and open a pull request describing what changed and why.
If you cannot complete the task, stop, commit what you have, and explain what is blocking you.";

/// How much of the operator's own Claude Code configuration a run inherits
/// (ADR-0004's amendment, applied by task 008).
///
/// An enum rather than the stored string, so the two spellings are compared once
/// here instead of at every call site. [`Inherit`](RunEnvironment::Inherit) is
/// the default and has no seeded row: an absent key *is* `inherit`, which is
/// also why there is no third `unset` variant.
/// What inheriting the operator's environment adds to a run, in dollars.
///
/// From `spike/FINDINGS.md` §2, which spawned the *same one-word prompt* twice:
/// $0.1061 inherited against $0.0291 isolated, on 16,455 cache-creation tokens
/// against 3,179. The difference is these ~13,300 tokens of tools, MCP servers
/// and hooks loaded before the run reads its plan.
///
/// # It is a fixed cost, and the ratio is the misleading way to say it
///
/// The spike reported "3.6x", and that number is true only of the trivial
/// prompt it was measured on, where setup *was* the whole run. This is charged
/// once per session as cache creation, not per turn, so it does not scale with
/// the work: the same ~$0.08 lands on a four-turn run and a forty-turn one. As
/// a share of a real run it has been observed anywhere from 64% (a ten-cent
/// metadata edit) to 0.2% (a $32 implementation).
///
/// Quoting the ratio in the UI therefore argues for `strict_local`, which is
/// the opposite of what the spike concluded — it recommended inheriting by
/// default, because reaching your own MCP servers mid-run is much of the point
/// of a local desktop app. So the UI states the fixed cost and puts it in
/// proportion against runs this installation has actually paid for.
pub const ENVIRONMENT_SETUP_COST_USD: f64 = 0.077;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEnvironment {
    /// The operator's MCP servers, hooks and plugins are capability worth
    /// having, and inheriting them is what makes a run behave like the user's
    /// own Claude Code. It costs roughly 3.6x per run, which is why the toggle
    /// surfaces per-run cost next to it.
    #[default]
    Inherit,
    /// `--strict-mcp-config --setting-sources project,local`.
    StrictLocal,
}

impl RunEnvironment {
    /// The stored spelling, which is also the wire spelling — one string, so a
    /// settings row stays legible in the sqlite3 CLI (ADR-0003).
    pub const fn as_str(self) -> &'static str {
        match self {
            RunEnvironment::Inherit => "inherit",
            RunEnvironment::StrictLocal => "strict_local",
        }
    }

    /// Reads a stored value, falling back to the default for anything else.
    ///
    /// Tolerant rather than fallible for the reason CLAUDE.md gives about CLI
    /// output: `settings` has no `CHECK` on `value` and the user is a supported
    /// writer of this file, so a typo hand-edited into the row must cost the
    /// safer default and a log line, never an overnight queue. It is also why
    /// this needs no error code — seam-contract D8 keeps [`crate::ErrorCode`]
    /// closed.
    fn from_stored(value: &str) -> Self {
        match value {
            "inherit" => RunEnvironment::Inherit,
            "strict_local" => RunEnvironment::StrictLocal,
            other => {
                tracing::warn!(
                    value = other,
                    "unrecognised run_environment; falling back to inherit"
                );
                RunEnvironment::default()
            }
        }
    }
}

/// The stored value for `key`, or `None` when no row has ever been written.
///
/// Takes a pool rather than a [`ServiceContext`] because a read has nothing to
/// publish and nothing to time, and startup wants one before a context exists.
/// Prefer the typed readers below — they are where an absent key gets its
/// meaning.
pub async fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>> {
    let value = sqlx::query_scalar!("SELECT value FROM settings WHERE key = ?1", key)
        .fetch_optional(pool)
        .await?;
    Ok(value)
}

/// Writes `key`, creating the row or replacing the value, and announces it.
///
/// One statement, so the `execute` *is* the commit; the publication still
/// follows it rather than preceding it, because ADR-0018's rule is about what a
/// subscriber can read when it re-reads.
pub async fn set(ctx: &ServiceContext, key: &str, value: &str) -> Result<()> {
    sqlx::query!(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        key,
        value,
    )
    .execute(&ctx.pool)
    .await?;

    ctx.publish(ChangeEvent::Settings);
    Ok(())
}

/// The global base instructions, or the empty string when the key is absent.
///
/// Empty and absent mean the same thing on purpose: both compose a prompt with
/// no base-instructions section (ADR-0009). Notably *not*
/// [`DEFAULT_BASE_INSTRUCTIONS`] — the seed is the migration's job, and handing
/// the default back on a read would quietly undo a user who cleared the field.
pub async fn base_instructions(pool: &SqlitePool) -> Result<String> {
    Ok(get(pool, BASE_INSTRUCTIONS).await?.unwrap_or_default())
}

pub async fn set_base_instructions(ctx: &ServiceContext, value: &str) -> Result<()> {
    set(ctx, BASE_INSTRUCTIONS, value).await
}

/// How much configuration a run inherits. Absent means
/// [`RunEnvironment::Inherit`], which is ADR-0004's amendment's default.
pub async fn run_environment(pool: &SqlitePool) -> Result<RunEnvironment> {
    Ok(get(pool, RUN_ENVIRONMENT)
        .await?
        .as_deref()
        .map(RunEnvironment::from_stored)
        .unwrap_or_default())
}

pub async fn set_run_environment(ctx: &ServiceContext, value: RunEnvironment) -> Result<()> {
    set(ctx, RUN_ENVIRONMENT, value.as_str()).await
}

/// Whether the user has already been through, or deliberately skipped, task
/// 018's first-run walkthrough.
///
/// Absent means "not yet", which is why the frontend's opening view is
/// *derived* — no registered repositories **and** this key absent — rather than
/// read off a flag alone. Derived self-heals: a user who registers a repository
/// some other way is not sent back to a welcome screen that has nothing left to
/// teach them. The key is what stops someone who deliberately skipped from
/// meeting the screen again on every launch, which the derivation alone cannot
/// express.
///
/// Anything other than the string this module writes reads as `false`, the same
/// tolerance every other key here applies: a hand-edited row is a reason to show
/// one extra screen, never a reason to fail a launch.
pub async fn onboarding_dismissed(pool: &SqlitePool) -> Result<bool> {
    Ok(get(pool, ONBOARDING_DISMISSED).await?.as_deref() == Some("true"))
}

pub async fn set_onboarding_dismissed(ctx: &ServiceContext, value: bool) -> Result<()> {
    set(
        ctx,
        ONBOARDING_DISMISSED,
        if value { "true" } else { "false" },
    )
    .await
}

/// What the user pays for their Claude subscription each month, or `None`.
///
/// **`None` is the answer the page needs**, not `0.0`: task 024 renders the
/// comparison only once there is a figure to compare against, and presents it
/// as *the user's own* because Rimaia cannot verify it.
///
/// A stored value that is not a number, or is negative, reads as absent — the
/// `run_environment` tolerance applied to a figure: a hand-edited row costs a
/// warning and a missing panel, never a page that will not open.
pub async fn subscription_monthly_usd(pool: &SqlitePool) -> Result<Option<f64>> {
    let Some(raw) = get(pool, SUBSCRIPTION_MONTHLY_USD).await? else {
        return Ok(None);
    };

    match raw.trim().parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(Some(value)),
        _ => {
            tracing::warn!(
                value = raw,
                "unreadable subscription_monthly_usd; treating it as not set"
            );
            Ok(None)
        }
    }
}

/// Stores it, or clears it.
///
/// Refuses a negative or non-finite figure rather than storing one the reader
/// would then have to ignore: this arrives from a form, and the place to say
/// "that is not a monthly cost" is at the field.
pub async fn set_subscription_monthly_usd(ctx: &ServiceContext, value: Option<f64>) -> Result<()> {
    match value {
        Some(value) if !value.is_finite() || value < 0.0 => Err(Error::invalid(
            "a monthly subscription cost has to be zero or more",
        )),
        Some(value) => set(ctx, SUBSCRIPTION_MONTHLY_USD, &value.to_string()).await,
        // Cleared rather than deleted: the key/value table has no delete, and
        // an empty string reads as absent through the parser above.
        None => set(ctx, SUBSCRIPTION_MONTHLY_USD, "").await,
    }
}

/// One doctor warning the user has read and decided about (task 027).
///
/// **Keyed on the row's content, not on its check.** `RepositoryPath` warns
/// about a *named* repository, so "I know about that one" must not silence the
/// same check firing about a different one; and `detail` is the sentence that
/// changes when the underlying condition does, so a `claude` upgraded from one
/// too-old version to another too-old version is a warning the user has not
/// seen yet. A dismissal is an answer to a specific sentence rather than a mute
/// button on a check.
///
/// Deliberately carries no status. Whether a row *may* be dismissed is
/// [`DoctorReport`](crate::doctor::DoctorReport)'s to decide when it marks, and
/// it only ever marks a `warn` — a stored dismissal naming a row that has since
/// turned into a `fail` marks nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dismissal {
    pub check: Check,
    /// The repository the row was about, for the two per-repository checks;
    /// `None` for the six that describe the installation as a whole.
    pub repository: Option<String>,
    pub detail: String,
}

/// Every dismissal the user has recorded, in the order they recorded them.
///
/// Tolerant of a hand-edited row for the reason [`RunEnvironment::from_stored`]
/// is: `settings` has no `CHECK` on `value`, the user is a supported writer of
/// this file (ADR-0003), and a typo in a *presentation* preference must never
/// cost a launch. Tolerant twice over, because the two failures are different
/// sizes — a value that is not an array at all falls back to "nothing
/// dismissed", and one unparseable element is skipped while the rest stand.
pub async fn doctor_dismissals(pool: &SqlitePool) -> Result<Vec<Dismissal>> {
    let Some(raw) = get(pool, DOCTOR_DISMISSALS).await? else {
        return Ok(Vec::new());
    };

    let elements: Vec<serde_json::Value> = match serde_json::from_str(&raw) {
        Ok(elements) => elements,
        Err(error) => {
            tracing::warn!(
                %error,
                "unreadable doctor_dismissals; treating it as nothing dismissed"
            );
            return Ok(Vec::new());
        }
    };

    Ok(elements
        .into_iter()
        .filter_map(|element| match serde_json::from_value(element) {
            Ok(dismissal) => Some(dismissal),
            Err(error) => {
                tracing::warn!(%error, "skipping an unreadable doctor dismissal");
                None
            }
        })
        .collect())
}

pub async fn set_doctor_dismissals(ctx: &ServiceContext, value: &[Dismissal]) -> Result<()> {
    // Mapped rather than unwrapped, the way `strategy::settings` writes its
    // own JSON key: seam-contract D8 keeps `ErrorCode` closed, so a failure
    // that cannot happen for this shape still travels as `Internal` instead of
    // as a panic in a settings write.
    let json = serde_json::to_string(value).map_err(|error| {
        Error::internal(format!("a doctor dismissal did not serialize: {error}"))
    })?;

    set(ctx, DOCTOR_DISMISSALS, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn the_migration_seeds_the_default_base_instructions_byte_for_byte() {
        // The pin that keeps `DEFAULT_BASE_INSTRUCTIONS` and the migration's SQL
        // literal from drifting. Asserted as one exact string rather than by
        // substring, because "restore the default" in Settings has to produce
        // the same bytes a first launch did.
        let pool = test_pool().await;

        assert_eq!(
            base_instructions(&pool).await.expect("read the seed"),
            DEFAULT_BASE_INSTRUCTIONS
        );
    }

    #[tokio::test]
    async fn the_seeded_default_is_the_four_sentences_task_006_specifies() {
        // The other half of the pin: the constant itself, spelled out here so a
        // reworded instruction has to be a deliberate edit in two places.
        let pool = test_pool().await;

        assert_eq!(
            base_instructions(&pool).await.expect("read the seed"),
            "Commit as you work, with focused commits and clear messages.\n\
             Run the project's tests and linters before you finish.\n\
             When the work is complete, push the branch and open a pull request describing what changed and why.\n\
             If you cannot complete the task, stop, commit what you have, and explain what is blocking you."
        );
    }

    #[tokio::test]
    async fn base_instructions_a_user_cleared_stay_cleared() {
        // The visible consequence of seeding in a migration instead of at every
        // launch. If this ever starts failing, someone has added an
        // insert-if-absent and the migration's header is now a lie.
        let h = TestContext::new().await;

        set_base_instructions(&h.context, "")
            .await
            .expect("clear the field");

        assert_eq!(
            base_instructions(&h.context.pool)
                .await
                .expect("read it back"),
            ""
        );
    }

    #[tokio::test]
    async fn an_edited_value_replaces_the_seed_rather_than_adding_a_row() {
        let h = TestContext::new().await;

        set_base_instructions(&h.context, "Open a draft PR, never a ready one.")
            .await
            .expect("edit the field");

        assert_eq!(
            base_instructions(&h.context.pool)
                .await
                .expect("read it back"),
            "Open a draft PR, never a ready one."
        );
        let rows: i64 = sqlx::query_scalar!(
            "SELECT count(*) FROM settings WHERE key = ?1",
            BASE_INSTRUCTIONS
        )
        .fetch_one(&h.context.pool)
        .await
        .expect("count the rows");
        assert_eq!(rows, 1, "the upsert must replace, never accumulate");
    }

    #[tokio::test]
    async fn an_unseeded_run_environment_reads_as_inherit() {
        let pool = test_pool().await;

        assert_eq!(
            get(&pool, RUN_ENVIRONMENT).await.expect("read the key"),
            None
        );
        assert_eq!(
            run_environment(&pool).await.expect("read the default"),
            RunEnvironment::Inherit
        );
    }

    #[tokio::test]
    async fn a_stored_run_environment_round_trips_through_its_spelling() {
        let h = TestContext::new().await;

        set_run_environment(&h.context, RunEnvironment::StrictLocal)
            .await
            .expect("store strict_local");

        assert_eq!(
            get(&h.context.pool, RUN_ENVIRONMENT)
                .await
                .expect("read the row"),
            Some("strict_local".to_string()),
            "the stored spelling has to stay legible in the sqlite3 CLI"
        );
        assert_eq!(
            run_environment(&h.context.pool)
                .await
                .expect("read it back"),
            RunEnvironment::StrictLocal
        );
    }

    #[tokio::test]
    async fn a_hand_edited_run_environment_falls_back_to_inherit_instead_of_failing() {
        // The row a user typed into the sqlite3 CLI. A queue must survive it.
        let h = TestContext::new().await;

        set(&h.context, RUN_ENVIRONMENT, "strictlocal")
            .await
            .expect("store a typo");

        assert_eq!(
            run_environment(&h.context.pool)
                .await
                .expect("read it back"),
            RunEnvironment::Inherit
        );
    }

    #[tokio::test]
    async fn writing_a_setting_publishes_settings_after_the_row_lands() {
        let mut h = TestContext::new().await;

        set_base_instructions(&h.context, "Run the linters.")
            .await
            .expect("write the field");

        assert_eq!(
            h.changes.try_recv().expect("a publication"),
            ChangeEvent::Settings
        );
    }

    #[test]
    fn run_environment_serializes_with_the_spelling_it_stores() {
        // One string for the column, the wire and the accessor — the same
        // three-way agreement `db::models` asserts for every other enum.
        for value in [RunEnvironment::Inherit, RunEnvironment::StrictLocal] {
            assert_eq!(
                serde_json::to_value(value).expect("an enum must serialize"),
                serde_json::Value::String(value.as_str().to_string())
            );
            assert_eq!(RunEnvironment::from_stored(value.as_str()), value);
        }
    }

    #[test]
    fn inherit_is_the_default_the_absent_key_stands_for() {
        assert_eq!(RunEnvironment::default(), RunEnvironment::Inherit);
    }
}
