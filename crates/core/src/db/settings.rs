//! The typed view of the settings stores (seam-contract D3, D28 part 4).
//!
//! [`Setting`](crate::db::Setting) is the row and nothing more. *Which* keys
//! exist, what each one holds, and what an absent key means are business rules,
//! and they live here so that every reader gets the same answer — task 008 in
//! particular reads [`run_environment`] through this module rather than through
//! SQL of its own, so `inherit | strict_local` is parsed in one place instead of
//! two.
//!
//! # Three stores, one placement
//!
//! [`placement`] says where each key lives (ADR-0028 point 2), and every read
//! and write goes through the accessor pair for that placement, which refuses
//! a key of any other: [`get_team`]/[`set_team`] over `team_settings`, for one
//! team in the context's scope; [`get_user`]/[`set_user`] over
//! `user_settings`, for the context's actor; and [`get_runner`]/[`set_runner`]
//! over the machine store, `runner.db`'s `runner_settings` (task 041), through
//! a [`MachineContext`] rather than the board's context. A key written through
//! a service never touches its legacy `settings` row again; those rows stay,
//! unread, for task 065 to drop with the table.
//!
//! Writes publish [`Change::Settings`](crate::events::Change::Settings) after the row is committed
//! (ADR-0018). The event carries no key: the whole table is a handful of rows
//! and every consumer re-reads all of it.

use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

use crate::context::ServiceContext;
use crate::db::Setting;
use crate::doctor::Check;
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::machine::MachineContext;

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

/// The keys that belong to one machine rather than to a team or a person
/// (seam-contract D28 part 4): what this computer runs, when, and how.
///
/// Task 038's migration leaves these out of `team_settings`, spelled in SQL as
/// this list is spelled here, task 040 copies them into `runner.db`, and from
/// task 041 every reader and writer reaches them through [`get_runner`] and
/// [`set_runner`].
/// `every_settings_key_has_the_placement_the_migration_gave_it` is what keeps
/// the two spellings from drifting.
pub const RUNNER_KEYS: [&str; 10] = [
    RUN_ENVIRONMENT,
    crate::mcp::settings::MCP_PORT,
    crate::scheduler::capacity::MAX_CONCURRENCY,
    crate::scheduler::capacity::SCHEDULE_MODE,
    crate::scheduler::state::QUEUE_STATE,
    crate::schedule::window::ACTIVE_RUN_WINDOW,
    crate::scheduler::pause::USAGE_LIMIT_PAUSE_UNTIL,
    crate::worktree::cleanup::AUTO_CLEANUP,
    DOCTOR_DISMISSALS,
    ONBOARDING_DISMISSED,
];

/// The keys that belong to one person (D28 part 4, and its 2026-10-04
/// amendment for task 034's digest marker). Task 038's migration copies them
/// into `user_settings`.
pub const USER_KEYS: [&str; 2] = [
    SUBSCRIPTION_MONTHLY_USD,
    crate::review::digest::REVIEW_DIGEST_SEEN_THROUGH,
];

/// Every key constant in the crate, for the tests that walk them all: 038's
/// placement test, and the one that reads each key back from the store its
/// placement names. A repository's strategy default has no constant (D17.2),
/// so each test names one for its own repository.
#[cfg(any(test, feature = "testing"))]
pub const ALL_KEYS: [&str; 20] = [
    BASE_INSTRUCTIONS,
    crate::strategy::catalogue::STRATEGY_CATALOGUE,
    crate::strategy::settings::STRATEGY_DEFAULT,
    crate::strategy::settings::STRATEGY_APPROVAL,
    crate::runner::process::MAX_TURNS,
    crate::runner::process::DISALLOWED_TOOLS,
    crate::review_loop::config::REVIEW_INSTRUCTIONS,
    crate::review_loop::config::REVIEW_CONFIG,
    SUBSCRIPTION_MONTHLY_USD,
    crate::review::digest::REVIEW_DIGEST_SEEN_THROUGH,
    RUN_ENVIRONMENT,
    crate::mcp::settings::MCP_PORT,
    crate::scheduler::capacity::MAX_CONCURRENCY,
    crate::scheduler::capacity::SCHEDULE_MODE,
    crate::scheduler::state::QUEUE_STATE,
    crate::schedule::window::ACTIVE_RUN_WINDOW,
    crate::scheduler::pause::USAGE_LIMIT_PAUSE_UNTIL,
    crate::worktree::cleanup::AUTO_CLEANUP,
    DOCTOR_DISMISSALS,
    ONBOARDING_DISMISSED,
];

/// Where a settings key lives once `settings` is split (ADR-0028 point 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Placement {
    /// `team_settings`. Every key not listed elsewhere.
    Team,
    /// `user_settings`: one of [`USER_KEYS`].
    User,
    /// `runner.db`'s `runner_settings`, behind the machine store (tasks 040
    /// and 041): one of [`RUNNER_KEYS`].
    Runner,
}

/// Which table `key` belongs in.
///
/// Team is the exclusion, not a list, so a key nobody placed ends up with the
/// team rather than being lost when task 065 drops `settings`. That covers
/// `strategy_default.<repository_id>` (D17.2) without a pattern, and every key
/// a later task adds without anyone remembering to list it; a key that
/// belongs to a person or a machine has to be listed above.
pub fn placement(key: &str) -> Placement {
    if USER_KEYS.contains(&key) {
        Placement::User
    } else if RUNNER_KEYS.contains(&key) {
        Placement::Runner
    } else {
        Placement::Team
    }
}

/// How much of the operator's own configuration a run inherits (ADR-0004's
/// amendment, applied by task 008).
///
/// An enum rather than the stored string, so the two spellings are compared once
/// here instead of at every call site. [`Inherit`](RunEnvironment::Inherit) is
/// the default and has no seeded row: an absent key *is* `inherit`, which is
/// also why there is no third `unset` variant.
///
/// What inheriting costs, in dollars, is a *provider's* answer, not a fact
/// this module states — see
/// [`AgentProvider::inherit_cost_usd`](crate::runner::provider::AgentProvider::inherit_cost_usd)
/// (task 032). `runner::provider::claude::INHERIT_COST_USD` carries the
/// measurement and the argument this doc used to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunEnvironment {
    /// The operator's MCP servers, hooks and plugins are capability worth
    /// having, and inheriting them is what makes a run behave like the
    /// operator's own interactive session. What it costs is the active
    /// provider's own answer — see this enum's own doc.
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

/// `Internal` for an accessor handed a key of another placement.
///
/// A wiring bug, never something a user did: every key's placement is fixed
/// by [`placement`], and each accessor serves one store.
fn ensure_placed(key: &str, expected: Placement) -> Result<()> {
    let actual = placement(key);
    if actual != expected {
        return Err(Error::internal(format!(
            "`{key}` is a {actual:?} setting, so it cannot be read or written as a {expected:?} one"
        )));
    }
    Ok(())
}

/// `NotFound` for a team outside the context's scope, in the sentence a team
/// that was never created would get: a team the caller cannot reach is not
/// there, as far as it can tell (ADR-0035 point 2).
fn ensure_team_in_scope(ctx: &ServiceContext, team_id: &str) -> Result<()> {
    if !ctx.scope.contains(team_id) {
        return Err(Error::not_found(format!("no team with id {team_id}")));
    }
    Ok(())
}

/// One team's value for a team key (ADR-0028 point 2), or `None` when that
/// team never wrote it.
pub(crate) async fn get_team(
    ctx: &ServiceContext,
    team_id: &str,
    key: &str,
) -> Result<Option<String>> {
    ensure_placed(key, Placement::Team)?;
    ensure_team_in_scope(ctx, team_id)?;
    let value = sqlx::query_scalar!(
        "SELECT value FROM team_settings WHERE team_id = ?1 AND key = ?2",
        team_id,
        key,
    )
    .fetch_optional(&ctx.pool)
    .await?;
    Ok(value)
}

/// Writes one team's value for a team key, or removes it with `None`, and
/// announces the change to that team.
///
/// [`set_team_in`] run over the pool: one statement, so the `execute` *is* the
/// commit; the publication still follows it, because ADR-0018's rule is about
/// what a subscriber can read when it re-reads.
pub(crate) async fn set_team(
    ctx: &ServiceContext,
    team_id: &str,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    let mut conn = ctx.pool.acquire().await?;
    set_team_in(ctx, &mut conn, team_id, key, value).await?;
    drop(conn);

    ctx.publish(ChangeEvent::settings(team_id.to_string()));
    Ok(())
}

/// [`set_team`]'s statement, on a connection the caller's transaction holds,
/// publishing nothing: the caller announces after its own commit. A save that
/// writes two keys together (the review loop's instructions and configuration)
/// commits them as one and announces once.
///
/// **The one statement that writes `team_settings`.** Task 045 adds the
/// revision and authorship columns here and task 051 the owner check, so a key
/// added later inherits both without anyone having to remember them. Removal
/// comes here too, for the same reason: a delete that bypassed this function
/// would be a write those checks never see. Its callers today are the review
/// loop's save and a repository's strategy default leaving inside its
/// repository's removal (D17.1).
pub(crate) async fn set_team_in(
    ctx: &ServiceContext,
    conn: &mut SqliteConnection,
    team_id: &str,
    key: &str,
    value: Option<&str>,
) -> Result<()> {
    ensure_placed(key, Placement::Team)?;
    ensure_team_in_scope(ctx, team_id)?;
    match value {
        Some(value) => {
            sqlx::query!(
                "INSERT INTO team_settings (team_id, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT (team_id, key) DO UPDATE SET value = excluded.value",
                team_id,
                key,
                value,
            )
            .execute(&mut *conn)
            .await?;
        }
        None => {
            sqlx::query!(
                "DELETE FROM team_settings WHERE team_id = ?1 AND key = ?2",
                team_id,
                key,
            )
            .execute(&mut *conn)
            .await?;
        }
    }
    Ok(())
}

/// The context's actor's value for a user key (ADR-0030 point 8), whatever the
/// scope, or `None` when they never wrote it.
pub(crate) async fn get_user(ctx: &ServiceContext, key: &str) -> Result<Option<String>> {
    let mut conn = ctx.pool.acquire().await?;
    get_user_in(ctx, &mut conn, key).await
}

/// Writes the context's actor's value for a user key, and announces it.
///
/// The event names the context's one team, read before the write so a scope
/// of several is refused untouched: a change event needs a team until task
/// 048's `Audience::User` gives a person's own settings an audience of their
/// own, and that is where this refusal ends.
pub(crate) async fn set_user(ctx: &ServiceContext, key: &str, value: &str) -> Result<()> {
    let team_id = ctx.scope.sole()?.clone();
    let mut conn = ctx.pool.acquire().await?;
    set_user_in(ctx, &mut conn, key, value).await?;
    drop(conn);

    ctx.publish(ChangeEvent::settings(team_id));
    Ok(())
}

/// [`get_user`]'s statement, on a connection the caller's transaction holds.
///
/// Takes the context for its actor and nothing else, never its pool: the read
/// belongs to the caller's transaction (a review verdict, `mark_seen`).
pub(crate) async fn get_user_in(
    ctx: &ServiceContext,
    conn: &mut SqliteConnection,
    key: &str,
) -> Result<Option<String>> {
    ensure_placed(key, Placement::User)?;
    let value = sqlx::query_scalar!(
        "SELECT value FROM user_settings WHERE user_id = ?1 AND key = ?2",
        ctx.actor,
        key,
    )
    .fetch_optional(&mut *conn)
    .await?;
    Ok(value)
}

/// [`set_user`]'s statement, on a connection the caller's transaction holds,
/// publishing nothing: the caller announces after its own commit.
///
/// **The one statement that writes `user_settings`.** The row is always the
/// context's actor's, never the row of whoever triggered a run or owns a
/// card, so a teammate's review never moves another person's marker.
pub(crate) async fn set_user_in(
    ctx: &ServiceContext,
    conn: &mut SqliteConnection,
    key: &str,
    value: &str,
) -> Result<()> {
    ensure_placed(key, Placement::User)?;
    sqlx::query!(
        "INSERT INTO user_settings (user_id, key, value) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, key) DO UPDATE SET value = excluded.value",
        ctx.actor,
        key,
        value,
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// One runner key's value, from the machine store, or `None` when this
/// machine never wrote it.
///
/// Storage only: what an absent or unreadable value means is decided by the
/// accessor that owns the key, next to the code with the rules (D3).
pub(crate) async fn get_runner(machine: &MachineContext, key: &str) -> Result<Option<String>> {
    ensure_placed(key, Placement::Runner)?;
    machine.store.get_setting(key).await
}

/// Writes one runner key into the machine store, and announces it.
pub(crate) async fn set_runner(machine: &MachineContext, key: &str, value: &str) -> Result<()> {
    ensure_placed(key, Placement::Runner)?;
    machine.store.set_setting(key, value).await?;

    // A machine event on the board's channel, under `event_team`, until task
    // 048 gives machine events `LocalEvents`.
    machine.publish(ChangeEvent::settings);
    Ok(())
}

/// Removes one runner key from the machine store, so it reads as absent, and
/// announces it.
pub(crate) async fn clear_runner(machine: &MachineContext, key: &str) -> Result<()> {
    ensure_placed(key, Placement::Runner)?;
    machine.store.clear_setting(key).await?;

    // A machine event on the board's channel, under `event_team`, until task
    // 048 gives machine events `LocalEvents`.
    machine.publish(ChangeEvent::settings);
    Ok(())
}

/// Every runner-placed row of the legacy `settings` table: what task 040's
/// adoption copies into `runner.db`.
///
/// Filtered through [`placement`] rather than a key list of its own, so the
/// copy cannot drift from what 038's migration left out of `team_settings`.
/// A key the board does not hold is not returned: absent already means "the
/// default" to every accessor, and the copy keeps it absent.
///
/// It takes the board's context and ignores its scope: machine state belongs
/// to no team. Used only by task 040's adoption step; nothing else reads the
/// board's copies of these keys. Task 065 deletes this with the table.
pub async fn runner_placed(ctx: &ServiceContext) -> Result<Vec<Setting>> {
    let rows = sqlx::query_as!(Setting, "SELECT key, value FROM settings ORDER BY key")
        .fetch_all(&ctx.pool)
        .await?;
    Ok(rows
        .into_iter()
        .filter(|row| placement(&row.key) == Placement::Runner)
        .collect())
}

/// The base instructions of the context's one team, or the empty string when
/// the key is absent.
///
/// Empty and absent mean the same thing on purpose: both compose a prompt with
/// no base-instructions section (ADR-0009). Notably *not*
/// [`DEFAULT_BASE_INSTRUCTIONS`] — the seed is the team's creation's job, and
/// handing the default back on a read would quietly undo a user who cleared
/// the field.
pub async fn base_instructions(ctx: &ServiceContext) -> Result<String> {
    base_instructions_for(ctx, ctx.scope.sole()?).await
}

/// One team's base instructions: what a run of that team's task composes
/// with, whichever teams the context reaches.
pub async fn base_instructions_for(ctx: &ServiceContext, team_id: &str) -> Result<String> {
    Ok(get_team(ctx, team_id, BASE_INSTRUCTIONS)
        .await?
        .unwrap_or_default())
}

/// The base instructions a run of `task_id` composes with: its own team's,
/// after the task is looked up in the context's scope.
pub async fn base_instructions_for_task(ctx: &ServiceContext, task_id: &str) -> Result<String> {
    let team_id = crate::tasks::service::team_of(ctx, task_id).await?;
    base_instructions_for(ctx, &team_id).await
}

pub async fn set_base_instructions(ctx: &ServiceContext, value: &str) -> Result<()> {
    set_team(ctx, ctx.scope.sole()?, BASE_INSTRUCTIONS, Some(value)).await
}

/// How much configuration a run inherits. Absent means
/// [`RunEnvironment::Inherit`], which is ADR-0004's amendment's default.
///
/// This machine's own setting, read from the machine store.
pub async fn run_environment(machine: &MachineContext) -> Result<RunEnvironment> {
    Ok(get_runner(machine, RUN_ENVIRONMENT)
        .await?
        .as_deref()
        .map(RunEnvironment::from_stored)
        .unwrap_or_default())
}

pub async fn set_run_environment(machine: &MachineContext, value: RunEnvironment) -> Result<()> {
    set_runner(machine, RUN_ENVIRONMENT, value.as_str()).await
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
///
/// This machine's own setting, read from the machine store.
pub async fn onboarding_dismissed(machine: &MachineContext) -> Result<bool> {
    Ok(get_runner(machine, ONBOARDING_DISMISSED).await?.as_deref() == Some("true"))
}

pub async fn set_onboarding_dismissed(machine: &MachineContext, value: bool) -> Result<()> {
    set_runner(
        machine,
        ONBOARDING_DISMISSED,
        if value { "true" } else { "false" },
    )
    .await
}

/// What the context's actor pays for their Claude subscription each month, or
/// `None`. A user setting: answered under any scope (ADR-0030 point 8).
///
/// **`None` is the answer the page needs**, not `0.0`: task 024 renders the
/// comparison only once there is a figure to compare against, and presents it
/// as *the user's own* because Rimaia cannot verify it.
///
/// A stored value that is not a number, or is negative, reads as absent — the
/// `run_environment` tolerance applied to a figure: a hand-edited row costs a
/// warning and a missing panel, never a page that will not open.
pub async fn subscription_monthly_usd(ctx: &ServiceContext) -> Result<Option<f64>> {
    let Some(raw) = get_user(ctx, SUBSCRIPTION_MONTHLY_USD).await? else {
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

/// Stores it, or clears it, for the context's actor.
///
/// Refuses a negative or non-finite figure rather than storing one the reader
/// would then have to ignore: this arrives from a form, and the place to say
/// "that is not a monthly cost" is at the field. Refused under a context that
/// reaches several teams, by [`set_user`], until task 048's `Audience::User`.
pub async fn set_subscription_monthly_usd(ctx: &ServiceContext, value: Option<f64>) -> Result<()> {
    match value {
        Some(value) if !value.is_finite() || value < 0.0 => Err(Error::invalid(
            "a monthly subscription cost has to be zero or more",
        )),
        Some(value) => set_user(ctx, SUBSCRIPTION_MONTHLY_USD, &value.to_string()).await,
        // Cleared rather than deleted: the key/value table has no delete, and
        // an empty string reads as absent through the parser above.
        None => set_user(ctx, SUBSCRIPTION_MONTHLY_USD, "").await,
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
/// is: `runner_settings` has no `CHECK` on `value`, the user is a supported writer of
/// this file (ADR-0003), and a typo in a *presentation* preference must never
/// cost a launch. Tolerant twice over, because the two failures are different
/// sizes — a value that is not an array at all falls back to "nothing
/// dismissed", and one unparseable element is skipped while the rest stand.
///
/// This machine's own setting, read from the machine store.
pub async fn doctor_dismissals(machine: &MachineContext) -> Result<Vec<Dismissal>> {
    let Some(raw) = get_runner(machine, DOCTOR_DISMISSALS).await? else {
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

pub async fn set_doctor_dismissals(machine: &MachineContext, value: &[Dismissal]) -> Result<()> {
    // Mapped rather than unwrapped, the way `strategy::settings` writes its
    // own JSON key: seam-contract D8 keeps `ErrorCode` closed, so a failure
    // that cannot happen for this shape still travels as `Internal` instead of
    // as a panic in a settings write.
    let json = serde_json::to_string(value).map_err(|error| {
        Error::internal(format!("a doctor dismissal did not serialize: {error}"))
    })?;

    set_runner(machine, DOCTOR_DISMISSALS, &json).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    /// The legacy row for `key`, read past every accessor: what a test
    /// compares a split store against.
    async fn legacy_row(pool: &sqlx::SqlitePool, key: &str) -> Option<String> {
        sqlx::query_scalar!("SELECT value FROM settings WHERE key = ?1", key)
            .fetch_optional(pool)
            .await
            .expect("read the legacy row")
    }

    #[tokio::test]
    async fn the_migration_seeds_the_default_base_instructions_byte_for_byte() {
        // The pin that keeps `DEFAULT_BASE_INSTRUCTIONS` and the migration's SQL
        // literal from drifting. Asserted as one exact string rather than by
        // substring, because "restore the default" in Settings has to produce
        // the same bytes a first launch did. The migration seeds the legacy
        // row, which 038's adoption compares byte for byte; a new team's row is
        // seeded from the constant.
        let pool = test_pool().await;

        assert_eq!(
            legacy_row(&pool, BASE_INSTRUCTIONS).await.as_deref(),
            Some(DEFAULT_BASE_INSTRUCTIONS)
        );
    }

    #[tokio::test]
    async fn the_seeded_default_is_the_four_sentences_task_006_specifies() {
        // The other half of the pin: the constant itself, spelled out here so a
        // reworded instruction has to be a deliberate edit in two places.
        let h = TestContext::new().await;

        assert_eq!(
            base_instructions(&h.context).await.expect("read the seed"),
            "Commit as you work, with focused commits and clear messages.\n\
             Run the project's tests and linters before you finish.\n\
             When the work is complete, push the branch and open a pull request describing what changed and why.\n\
             If you cannot complete the task, stop, commit what you have, and explain what is blocking you."
        );
    }

    #[tokio::test]
    async fn base_instructions_a_user_cleared_stay_cleared() {
        // The visible consequence of seeding at the team's creation instead of
        // at every launch. If this ever starts failing, someone has added an
        // insert-if-absent and the seed's comment is now a lie.
        let h = TestContext::new().await;

        set_base_instructions(&h.context, "")
            .await
            .expect("clear the field");

        assert_eq!(
            base_instructions(&h.context).await.expect("read it back"),
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
            base_instructions(&h.context).await.expect("read it back"),
            "Open a draft PR, never a ready one."
        );
        let rows: i64 = sqlx::query_scalar!(
            "SELECT count(*) FROM team_settings WHERE team_id = ?1 AND key = ?2",
            h.solo.team_id,
            BASE_INSTRUCTIONS
        )
        .fetch_one(&h.context.pool)
        .await
        .expect("count the rows");
        assert_eq!(rows, 1, "the upsert must replace, never accumulate");
    }

    #[tokio::test]
    async fn a_team_setting_written_after_the_split_never_touches_the_legacy_table() {
        let h = TestContext::new().await;
        sqlx::query!(
            "INSERT INTO settings (key, value) VALUES (?1, 'stale legacy instructions')
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            BASE_INSTRUCTIONS
        )
        .execute(&h.context.pool)
        .await
        .expect("plant a stale legacy row");

        set_base_instructions(&h.context, "Open a draft PR.")
            .await
            .expect("write the team's value");

        assert_eq!(
            legacy_row(&h.context.pool, BASE_INSTRUCTIONS)
                .await
                .as_deref(),
            Some("stale legacy instructions"),
            "the legacy row is left for 065, unwritten"
        );
        assert_eq!(
            base_instructions(&h.context).await.expect("read it back"),
            "Open a draft PR.",
            "and never read"
        );
    }

    #[tokio::test]
    async fn an_accessor_handed_a_key_of_another_placement_is_an_internal_error() {
        // A wiring bug, so it reads as one rather than as a refusal a user
        // could act on.
        let h = TestContext::new().await;
        let team = h.solo.team_id.clone();

        let mut refusals = vec![
            get_team(&h.context, &team, RUN_ENVIRONMENT).await.err(),
            set_team(&h.context, &team, SUBSCRIPTION_MONTHLY_USD, Some("1"))
                .await
                .err(),
            get_user(&h.context, BASE_INSTRUCTIONS).await.err(),
            set_user(&h.context, ONBOARDING_DISMISSED, "true")
                .await
                .err(),
            get_runner(h.machine(), BASE_INSTRUCTIONS).await.err(),
            set_runner(h.machine(), SUBSCRIPTION_MONTHLY_USD, "1")
                .await
                .err(),
            clear_runner(h.machine(), BASE_INSTRUCTIONS).await.err(),
        ];
        // The test pool holds one connection, so the two that take one go
        // last, on a connection held only for them.
        let mut conn = h.context.pool.acquire().await.expect("a connection");
        refusals.push(
            get_user_in(&h.context, &mut conn, RUN_ENVIRONMENT)
                .await
                .err(),
        );
        refusals.push(
            set_user_in(&h.context, &mut conn, BASE_INSTRUCTIONS, "x")
                .await
                .err(),
        );

        for refusal in refusals {
            assert_eq!(
                refusal.expect("a key of another placement").code(),
                crate::ErrorCode::Internal
            );
        }
    }

    #[tokio::test]
    async fn a_team_outside_the_scope_is_not_found_to_the_team_accessors() {
        let h = TestContext::new().await;
        let elsewhere = "3f2b1c00-0000-4000-8000-0000000000b2";

        for error in [
            get_team(&h.context, elsewhere, BASE_INSTRUCTIONS)
                .await
                .expect_err("a team the context cannot reach"),
            set_team(&h.context, elsewhere, BASE_INSTRUCTIONS, Some("x"))
                .await
                .expect_err("a team the context cannot reach"),
            set_team(&h.context, elsewhere, BASE_INSTRUCTIONS, None)
                .await
                .expect_err("a removal is a write like any other"),
        ] {
            assert_eq!(error.code(), crate::ErrorCode::NotFound);
            assert_eq!(error.to_string(), format!("no team with id {elsewhere}"));
        }
    }

    #[tokio::test]
    async fn an_unseeded_run_environment_reads_as_inherit() {
        let h = TestContext::new().await;

        assert_eq!(
            get_runner(h.machine(), RUN_ENVIRONMENT)
                .await
                .expect("read the key"),
            None
        );
        assert_eq!(
            run_environment(h.machine())
                .await
                .expect("read the default"),
            RunEnvironment::Inherit
        );
    }

    #[tokio::test]
    async fn a_stored_run_environment_round_trips_through_its_spelling() {
        let h = TestContext::new().await;

        set_run_environment(h.machine(), RunEnvironment::StrictLocal)
            .await
            .expect("store strict_local");

        assert_eq!(
            get_runner(h.machine(), RUN_ENVIRONMENT)
                .await
                .expect("read the row"),
            Some("strict_local".to_string()),
            "the stored spelling has to stay legible in the sqlite3 CLI"
        );
        assert_eq!(
            run_environment(h.machine()).await.expect("read it back"),
            RunEnvironment::StrictLocal
        );
    }

    #[tokio::test]
    async fn a_hand_edited_run_environment_falls_back_to_inherit_instead_of_failing() {
        // The row a user typed into the sqlite3 CLI. A queue must survive it.
        let h = TestContext::new().await;

        set_runner(h.machine(), RUN_ENVIRONMENT, "strictlocal")
            .await
            .expect("store a typo");

        assert_eq!(
            run_environment(h.machine()).await.expect("read it back"),
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
            ChangeEvent::settings(h.solo.team_id.clone())
        );
    }

    #[tokio::test]
    async fn removing_a_team_setting_deletes_its_row_and_announces_it() {
        // Removal goes through the one writer, so it is scoped and announced
        // exactly as a write is, and leaves no row behind (D17.1).
        let mut h = TestContext::new().await;
        let key = "strategy_default.3f2b1c00-0000-4000-8000-0000000000c3";
        set_team(&h.context, &h.solo.team_id, key, Some("{}"))
            .await
            .expect("write the row");
        h.changes.try_recv().expect("the write's publication");

        set_team(&h.context, &h.solo.team_id, key, None)
            .await
            .expect("remove the row");

        let rows = sqlx::query_scalar!(
            r#"SELECT count(*) AS "count!: i64" FROM team_settings WHERE key = ?1"#,
            key,
        )
        .fetch_one(&h.context.pool)
        .await
        .expect("count the rows");
        assert_eq!(rows, 0);
        assert_eq!(
            h.changes.try_recv().expect("the removal's publication"),
            ChangeEvent::settings(h.solo.team_id.clone())
        );
    }

    #[tokio::test]
    async fn runner_placed_returns_exactly_the_runner_rows_the_board_holds() {
        let h = TestContext::new().await;
        // Every runner key but one, plus a team and a user key planted in the
        // legacy table, so the filter has something on each side of it.
        let (absent, held) = RUNNER_KEYS.split_last().expect("ten runner keys");
        for key in held
            .iter()
            .chain([&BASE_INSTRUCTIONS, &SUBSCRIPTION_MONTHLY_USD])
        {
            sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)")
                .bind(key)
                .bind(format!("what {key} held"))
                .execute(&h.context.pool)
                .await
                .expect("plant a legacy row");
        }

        let rows = runner_placed(&h.context).await.expect("read the rows");

        let mut expected: Vec<Setting> = held
            .iter()
            .map(|key| Setting {
                key: (*key).to_string(),
                value: format!("what {key} held"),
            })
            .collect();
        expected.sort_by(|a, b| a.key.cmp(&b.key));
        assert_eq!(rows, expected);
        assert!(
            rows.iter().all(|row| row.key != *absent),
            "a key the board does not hold is not returned"
        );
    }

    #[test]
    fn every_settings_key_has_the_placement_the_migration_gave_it() {
        // Every key constant in the crate, spelled through the module that owns
        // it, against D28 part 4 and its 2026-10-04 amendment. A key a later
        // task adds without listing it lands with the team by exclusion; this
        // is where it has to be added, and where a person's or a machine's key
        // would be caught going to the team.
        use crate::review::digest::REVIEW_DIGEST_SEEN_THROUGH;
        use crate::review_loop::config::{REVIEW_CONFIG, REVIEW_INSTRUCTIONS};
        use crate::runner::process::{DISALLOWED_TOOLS, MAX_TURNS};
        use crate::strategy::catalogue::STRATEGY_CATALOGUE;
        use crate::strategy::settings::{
            repository_default_key, STRATEGY_APPROVAL, STRATEGY_DEFAULT,
        };

        let per_repository = repository_default_key("3f2b1c00-0000-4000-8000-0000000000a1");
        let expected = [
            (BASE_INSTRUCTIONS, Placement::Team),
            (STRATEGY_CATALOGUE, Placement::Team),
            (STRATEGY_DEFAULT, Placement::Team),
            (per_repository.as_str(), Placement::Team),
            (STRATEGY_APPROVAL, Placement::Team),
            (MAX_TURNS, Placement::Team),
            (DISALLOWED_TOOLS, Placement::Team),
            (REVIEW_INSTRUCTIONS, Placement::Team),
            (REVIEW_CONFIG, Placement::Team),
            (SUBSCRIPTION_MONTHLY_USD, Placement::User),
            (REVIEW_DIGEST_SEEN_THROUGH, Placement::User),
            (RUN_ENVIRONMENT, Placement::Runner),
            (crate::mcp::settings::MCP_PORT, Placement::Runner),
            (
                crate::scheduler::capacity::MAX_CONCURRENCY,
                Placement::Runner,
            ),
            (crate::scheduler::capacity::SCHEDULE_MODE, Placement::Runner),
            (crate::scheduler::state::QUEUE_STATE, Placement::Runner),
            (
                crate::schedule::window::ACTIVE_RUN_WINDOW,
                Placement::Runner,
            ),
            (
                crate::scheduler::pause::USAGE_LIMIT_PAUSE_UNTIL,
                Placement::Runner,
            ),
            (crate::worktree::cleanup::AUTO_CLEANUP, Placement::Runner),
            (DOCTOR_DISMISSALS, Placement::Runner),
            (ONBOARDING_DISMISSED, Placement::Runner),
        ];

        let placed: Vec<(&str, Placement)> = expected
            .iter()
            .map(|(key, _)| (*key, placement(key)))
            .collect();
        assert_eq!(placed, expected.to_vec());

        // The same list the store test walks, so a key added to one and not
        // the other is a failure here.
        let constants: Vec<&str> = expected
            .iter()
            .map(|(key, _)| *key)
            .filter(|key| *key != per_repository.as_str())
            .collect();
        assert_eq!(constants, ALL_KEYS.to_vec());

        // The spellings the migration's SQL lists, so a key moved between the
        // lists here without the SQL following is a failure, not a drift.
        assert_eq!(
            USER_KEYS,
            ["subscription_monthly_usd", "review_digest_seen_through"]
        );
        assert_eq!(
            RUNNER_KEYS,
            [
                "run_environment",
                "mcp_port",
                "max_concurrency",
                "schedule_mode",
                "queue_state",
                "active_run_window",
                "usage_limit_pause_until",
                "worktree_auto_cleanup",
                "doctor_dismissals",
                "onboarding_dismissed",
            ]
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
