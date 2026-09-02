//! How many runs the queue may have in flight, and how many of them may be in
//! any one repository (ADR-0010's Modes and Selection).
//!
//! # Two settings keys, not two `schedules` columns
//!
//! `schedules` has carried `mode` and `max_concurrency` since the initial
//! schema and nothing reads either; task 013 is what gives a *named schedule*
//! its own configuration. Task 012 needs the numbers now and takes them as
//! `settings` keys in the shape seam-contract D3 fixes and
//! [`state`](super::state) already uses for `queue_state`: storage through
//! [`db::settings`](crate::db::settings), the rules about the key here, in the
//! module that has the rules.
//!
//! That leaves a reconciliation task 013 inherits rather than discovers, and
//! the seam-contract entry names it: once a schedule can say "run this list in
//! parallel, three at a time", there are two answers to "what mode is the queue
//! in" — the active schedule's and this default. Neither is wrong; which one
//! wins while a window is open is a decision, and it is 013's.
//!
//! # Sequential resolves to one slot, whatever the number says
//!
//! [`resolve`] returns `global = 1` in [`ScheduleMode::Sequential`] regardless
//! of `max_concurrency`. That is what keeps sequential mode on literally the
//! same code path as parallel rather than on a preserved special case, and it
//! is what makes "turning parallelism on did not change sequential mode"
//! something a test can assert instead of something a reviewer has to believe.
//! The stored number is left alone, so flipping back to `parallel` restores the
//! value the user chose rather than a default.
//!
//! # Reads are tolerant, writes are strict
//!
//! An absent, unparseable or out-of-range stored value warns and falls back to
//! a Rust default — never fatal. The same rule
//! [`RunEnvironment::from_stored`](crate::db::settings::RunEnvironment),
//! [`QueueState::from_stored`](super::QueueState) and
//! [`mcp::settings::configured_port`](crate::mcp::settings::configured_port)
//! follow, for the reason ADR-0003 gives: the user is a supported writer of
//! this file, and a queue that refuses to run all night because of a typo in
//! the `sqlite3` CLI is a worse outcome than one that runs two at a time when
//! four were meant. The setters refuse out-of-range input with a sentence,
//! because a form and a tool are not the `sqlite3` CLI.

use std::collections::HashMap;

use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::{settings, ScheduleMode};
use crate::error::{Error, Result};
use crate::repo;
use crate::scheduler::inflight::CONCURRENCY_CEILING;

/// The `settings` key holding [`ScheduleMode`] for the queue's default
/// configuration.
pub const SCHEDULE_MODE: &str = "schedule_mode";

/// The `settings` key holding how many runs [`ScheduleMode::Parallel`] allows
/// at once.
pub const MAX_CONCURRENCY: &str = "max_concurrency";

/// ADR-0010's "up to `max_concurrency` runs at once (default 2)".
pub const DEFAULT_MAX_CONCURRENCY: usize = 2;

/// What a repository that has not opted out holds at once.
///
/// One, per ADR-0010, and it is the *whole* of the per-repository story until
/// somebody changes a row: "parallelism across repositories is the safe
/// default; within one repo it is opt-in."
pub const DEFAULT_PER_REPOSITORY: usize = 1;

/// The two numbers [`selection::next_batch`](super::selection::next_batch) and
/// [`InFlight::acquire`](super::InFlight::acquire) are checked against, for one
/// pass of the queue loop.
///
/// A snapshot taken once per pass rather than a value the scheduler holds: the
/// user may flip the mode at 23:00 with four runs in flight, and the pass after
/// that read is the one that has to notice. Nothing here is cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// How many runs may be in flight at once, anywhere.
    pub global: usize,
    /// Each repository's own cap, keyed by repository id. **A missing key means
    /// [`DEFAULT_PER_REPOSITORY`]**, not "unbounded" — the map carries every
    /// registered repository, and a task whose repository was deleted between
    /// the board read and this one must not become the one thing with no limit.
    pub per_repository: HashMap<String, usize>,
}

impl Resolved {
    /// This repository's cap, or the default for one the map does not name.
    pub fn for_repository(&self, repository_id: &str) -> usize {
        self.per_repository
            .get(repository_id)
            .copied()
            .unwrap_or(DEFAULT_PER_REPOSITORY)
    }
}

/// What the Settings control and the MCP tool read, in one call.
///
/// Carries [`ceiling`](Self::ceiling) as a value rather than leaving the client
/// to know [`CONCURRENCY_CEILING`]: a number input that has to bound itself
/// needs the bound, and a hard-coded `8` in TypeScript is a second copy of a
/// constant whose whole purpose is that there is one.
///
/// [`max_concurrency`](Self::max_concurrency) is the *stored* limit and not
/// what [`resolve`] would return — the two differ in sequential mode, on
/// purpose, and a panel that showed `1` there would make the number look as if
/// it had been forgotten every time the mode was flipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunCapacity {
    pub mode: ScheduleMode,
    pub max_concurrency: usize,
    pub ceiling: usize,
}

/// The queue's configured capacity, as stored.
pub async fn configured(pool: &SqlitePool) -> Result<RunCapacity> {
    Ok(RunCapacity {
        mode: schedule_mode(pool).await?,
        max_concurrency: max_concurrency(pool).await?,
        ceiling: CONCURRENCY_CEILING,
    })
}

/// Reads the mode and both limits, and resolves them into the numbers one pass
/// of the queue loop is bounded by.
pub async fn resolve(ctx: &ServiceContext) -> Result<Resolved> {
    let global = match schedule_mode(&ctx.pool).await? {
        // Not `max_concurrency.min(1)` and not a branch further down: this is
        // the whole of what sequential mode *is* now, and stating it here is
        // what stops a later reader looking for the second code path.
        ScheduleMode::Sequential => 1,
        ScheduleMode::Parallel => max_concurrency(&ctx.pool).await?,
    };

    let per_repository = repo::list(ctx)
        .await?
        .into_iter()
        .map(|repository| {
            (
                repository.id,
                usable_repository_concurrency(&repository.name, repository.max_concurrency),
            )
        })
        .collect();

    Ok(Resolved {
        global,
        per_repository,
    })
}

/// The queue's default mode. An absent key is [`ScheduleMode::Sequential`].
pub async fn schedule_mode(pool: &SqlitePool) -> Result<ScheduleMode> {
    Ok(settings::get(pool, SCHEDULE_MODE)
        .await?
        .as_deref()
        .map(mode_from_stored)
        .unwrap_or_default())
}

/// Writes the mode and announces it (ADR-0018: `settings:changed` is what tells
/// the Runs view and the Settings panel to re-read).
///
/// Total rather than fallible, unlike [`set_max_concurrency`]: an enum off the
/// wire has already been validated by serde, so there is no out-of-range value
/// left for this to refuse.
pub async fn set_schedule_mode(ctx: &ServiceContext, mode: ScheduleMode) -> Result<()> {
    settings::set(ctx, SCHEDULE_MODE, mode.as_str()).await
}

/// How many runs [`ScheduleMode::Parallel`] allows at once, clamped to
/// [`CONCURRENCY_CEILING`].
///
/// Clamped rather than refused on read, and clamped *here* rather than only in
/// [`InFlight::acquire`](super::InFlight::acquire), so that the number the
/// Settings panel shows and the number the queue obeys are the same one. The
/// registry still enforces the ceiling itself — two doors, one rule, no gap
/// between them.
pub async fn max_concurrency(pool: &SqlitePool) -> Result<usize> {
    let Some(stored) = settings::get(pool, MAX_CONCURRENCY).await? else {
        return Ok(DEFAULT_MAX_CONCURRENCY);
    };

    match stored.trim().parse::<usize>() {
        Ok(0) | Err(_) => {
            tracing::warn!(
                value = stored,
                default = DEFAULT_MAX_CONCURRENCY,
                "unusable max_concurrency; falling back to the default"
            );
            Ok(DEFAULT_MAX_CONCURRENCY)
        }
        Ok(value) if value > CONCURRENCY_CEILING => {
            tracing::warn!(
                value,
                ceiling = CONCURRENCY_CEILING,
                "max_concurrency is above the ceiling; using the ceiling"
            );
            Ok(CONCURRENCY_CEILING)
        }
        Ok(value) => Ok(value),
    }
}

/// Writes the global limit, refusing anything outside the range that has a
/// meaning.
///
/// "Zero" is not spelled here — a queue that starts nothing is
/// [`QueueState::Paused`](super::QueueState), which is a switch the user
/// already has and which the Runs view already explains. Two ways to stop the
/// queue would be two things to check when it is not running.
pub async fn set_max_concurrency(ctx: &ServiceContext, value: usize) -> Result<()> {
    if !(1..=CONCURRENCY_CEILING).contains(&value) {
        return Err(Error::invalid(format!(
            "Rimaia will supervise between 1 and {CONCURRENCY_CEILING} runs at once, not {value}. \
             To start nothing at all, pause the queue."
        )));
    }
    settings::set(ctx, MAX_CONCURRENCY, &value.to_string()).await
}

/// A stored mode, or the safe one for anything else. See the module header on
/// why the fallback direction is the narrow one.
fn mode_from_stored(value: &str) -> ScheduleMode {
    match value {
        "sequential" => ScheduleMode::Sequential,
        "parallel" => ScheduleMode::Parallel,
        other => {
            tracing::warn!(
                value = other,
                "unrecognised schedule_mode; falling back to sequential"
            );
            ScheduleMode::default()
        }
    }
}

/// A stored per-repository cap, held to the range that has a meaning.
///
/// The column is `NOT NULL DEFAULT 1`, so the only way to reach either arm is a
/// hand-edited row — which ADR-0003 says will happen. Naming the repository in
/// the warning matters here in a way it does not for the global keys: the
/// operator has to know *which* row to fix.
fn usable_repository_concurrency(name: &str, stored: i64) -> usize {
    if stored < 1 {
        tracing::warn!(
            repository = name,
            value = stored,
            "unusable repositories.max_concurrency; falling back to one run at a time"
        );
        return DEFAULT_PER_REPOSITORY;
    }

    let value = usize::try_from(stored).unwrap_or(CONCURRENCY_CEILING);
    if value > CONCURRENCY_CEILING {
        tracing::warn!(
            repository = name,
            value,
            ceiling = CONCURRENCY_CEILING,
            "repositories.max_concurrency is above the ceiling; using the ceiling"
        );
        return CONCURRENCY_CEILING;
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    #[tokio::test]
    async fn a_queue_nobody_has_configured_is_sequential_and_holds_one_run() {
        let harness = TestContext::new().await;

        assert_eq!(
            schedule_mode(&harness.context.pool).await.expect("read"),
            ScheduleMode::Sequential
        );
        assert_eq!(
            resolve(&harness.context).await.expect("resolve").global,
            1,
            "the default configuration is exactly what task 009 shipped"
        );
    }

    #[tokio::test]
    async fn sequential_mode_resolves_to_one_slot_whatever_max_concurrency_says() {
        // The property that keeps sequential mode on the same code path as
        // parallel rather than on a preserved special case. The stored number
        // is deliberately left alone, so flipping back to `parallel` restores
        // the four the user chose.
        let harness = TestContext::new().await;
        set_max_concurrency(&harness.context, 4)
            .await
            .expect("store a global limit");
        set_schedule_mode(&harness.context, ScheduleMode::Sequential)
            .await
            .expect("stay sequential");

        assert_eq!(resolve(&harness.context).await.expect("resolve").global, 1);
        assert_eq!(
            max_concurrency(&harness.context.pool).await.expect("read"),
            4,
            "the number the user chose is remembered, not overwritten",
        );

        set_schedule_mode(&harness.context, ScheduleMode::Parallel)
            .await
            .expect("turn parallelism on");
        assert_eq!(resolve(&harness.context).await.expect("resolve").global, 4);
    }

    #[tokio::test]
    async fn a_hand_edited_max_concurrency_is_clamped_to_the_ceiling() {
        // ADR-0010's "a configurable ceiling regardless of mode, so a mis-set
        // value cannot spawn ten agents" — enforced on the read as well as in
        // the registry, so the panel and the queue agree on the number.
        let harness = TestContext::new().await;
        settings::set(&harness.context, MAX_CONCURRENCY, "40")
            .await
            .expect("store a value no form would send");

        assert_eq!(
            max_concurrency(&harness.context.pool).await.expect("read"),
            CONCURRENCY_CEILING
        );
    }

    #[tokio::test]
    async fn a_hand_edited_max_concurrency_that_is_not_a_number_falls_back_rather_than_failing() {
        let pool = test_pool().await;
        let harness = TestContext::new().await;

        assert_eq!(
            max_concurrency(&pool).await.expect("an absent key"),
            DEFAULT_MAX_CONCURRENCY
        );

        for nonsense in ["two", "", "0", "-1", "2.5"] {
            settings::set(&harness.context, MAX_CONCURRENCY, nonsense)
                .await
                .expect("store nonsense");
            assert_eq!(
                max_concurrency(&harness.context.pool)
                    .await
                    .expect("a bad row is not an error"),
                DEFAULT_MAX_CONCURRENCY,
                "{nonsense:?}",
            );
        }
    }

    #[tokio::test]
    async fn a_hand_edited_mode_falls_back_to_sequential_rather_than_to_parallel() {
        // The direction of the fallback is the decision, exactly as it is for
        // `queue_state`: a typo must not widen what an unattended queue spawns.
        let harness = TestContext::new().await;
        settings::set(&harness.context, SCHEDULE_MODE, "Parallel")
            .await
            .expect("store a typo");

        assert_eq!(
            schedule_mode(&harness.context.pool).await.expect("read"),
            ScheduleMode::Sequential
        );
    }

    #[tokio::test]
    async fn a_limit_outside_the_range_is_refused_and_stores_nothing() {
        let harness = TestContext::new().await;

        for refused in [0, CONCURRENCY_CEILING + 1] {
            let error = set_max_concurrency(&harness.context, refused)
                .await
                .expect_err("a form must not be able to send this");
            assert!(
                error.to_string().contains("pause the queue"),
                "the refusal has to name the thing the user actually wants: {error}"
            );
        }

        assert_eq!(
            settings::get(&harness.context.pool, MAX_CONCURRENCY)
                .await
                .expect("read the key"),
            None,
            "a refused write must leave the row alone",
        );
    }

    #[test]
    fn the_mode_round_trips_through_the_spelling_it_stores() {
        for mode in [ScheduleMode::Sequential, ScheduleMode::Parallel] {
            assert_eq!(mode_from_stored(mode.as_str()), mode);
        }
    }

    #[test]
    fn a_repository_that_has_not_opted_out_is_capped_at_one() {
        // ADR-0010's per-repository rule, at the one place it is turned into a
        // number: the column's own `NOT NULL DEFAULT 1` is what a repository
        // nobody has touched carries.
        assert_eq!(usable_repository_concurrency("rimaia", 1), 1);
        assert_eq!(usable_repository_concurrency("rimaia", 3), 3);
    }

    #[test]
    fn a_hand_edited_repository_cap_is_clamped_at_both_ends() {
        for stored in [0, -1, i64::MIN] {
            assert_eq!(
                usable_repository_concurrency("rimaia", stored),
                DEFAULT_PER_REPOSITORY,
                "{stored}",
            );
        }
        for stored in [CONCURRENCY_CEILING as i64 + 1, i64::MAX] {
            assert_eq!(
                usable_repository_concurrency("rimaia", stored),
                CONCURRENCY_CEILING,
                "{stored}",
            );
        }
    }

    #[test]
    fn a_repository_the_map_does_not_name_still_has_a_limit() {
        // The failure this closes: a task whose repository was deleted between
        // the board read and the capacity read must not become the one thing
        // with no cap at all.
        let resolved = Resolved {
            global: 4,
            per_repository: HashMap::new(),
        };

        assert_eq!(resolved.for_repository("gone"), DEFAULT_PER_REPOSITORY);
    }
}
