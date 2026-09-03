//! The run window that is open right now: which schedule opened it, when it
//! closes, and what configuration it runs under (ADR-0010's run windows).
//!
//! # One `settings` key, in D3's shape
//!
//! `active_run_window` holds a JSON [`RunWindow`], stored through task 006's
//! accessor with the rules about the key in the module that has the rules —
//! exactly as [`scheduler::state`](crate::scheduler::state) does for
//! `queue_state`, [`capacity`](crate::scheduler::capacity) for `schedule_mode`
//! and [`pause`](crate::scheduler::pause) for the usage-limit hold. Seam-contract
//! D4 forbids a column, and a column would be wrong anyway: at most one window
//! is open, so this is a singleton fact about the installation, which is what the
//! `settings` table is.
//!
//! **Stored rather than held in memory**, for the same reason `pause` gives. A
//! window opened at 22:00 must still be open — and must still know it closes at
//! 06:00 — after a relaunch at 03:00. A field on the queue would forget both.
//!
//! # The D21 reconciliation, settled
//!
//! Seam-contract D21 handed task 013 a named problem: once a schedule can say
//! "run this list in parallel, three at a time", there are two answers to "what
//! mode is the queue in" — the active schedule's, and the `schedule_mode` /
//! `max_concurrency` settings default. **The open window wins; the default wins
//! whenever no window is open.** Three reasons, in order of weight:
//!
//! 1. **It is the more specific instruction, and the more recent act.** The user
//!    configured "Nightly: parallel, three" for this window deliberately, on a
//!    row they named. A default is what applies when nobody has said anything
//!    more specific — that is what the word means.
//! 2. **It is what makes ADR-0010's columns mean anything.** `schedules.mode`
//!    and `schedules.max_concurrency` have existed since the initial schema, and
//!    D21 point 1 took settings keys only because "a `schedules` row nothing
//!    selects from cannot supply them". One does now. Reading the row and then
//!    ignoring it would leave two columns that are written, rendered, and never
//!    obeyed — which is worse than not having them.
//! 3. **A manual Start opens no window**, so pressing the button still uses the
//!    default, and nothing about task 012's behaviour changes on any night
//!    nobody has scheduled. The button is not a schedule.
//!
//! And the consequence D21 asks about explicitly — what the Settings control
//! shows while a window is open — is **the stored default, unchanged**. That is
//! D21 point 2's own argument applied one layer out: the control already shows
//! the *stored* `max_concurrency` rather than the `1` sequential mode resolves
//! to, because a number that changed every time a mode was flipped would look
//! forgotten. A control that changed every time a window opened would be worse
//! still — it would read as the user's setting having been silently rewritten at
//! 22:00. Where "what is happening right now" belongs is the Runs view, which is
//! why [`QueueStatus`](crate::scheduler::QueueStatus) carries the window and the
//! Settings panel does not.
//!
//! # Closed by three things, and quitting is one of them
//!
//! [`close`] is called when the stop time arrives, by `pause` and `stop`, and by
//! the exit path. The last is seam-contract D15's amendment: quitting mid-window
//! closes the window, so relaunching at 03:00 does not silently resume a night
//! the user quit out of — while the schedule's *next* occurrence still fires,
//! because a schedule is a standing instruction and a window is not.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::{settings, Schedule, ScheduleMode};
use crate::error::Result;

/// The `settings` key holding the open [`RunWindow`], if there is one.
pub const ACTIVE_RUN_WINDOW: &str = "active_run_window";

/// The window a schedule opened, as stored.
///
/// Carries the schedule's *name* as well as its id, denormalised on purpose:
/// the Runs view says "Running until 06:00 — Nightly", and re-reading the
/// `schedules` row to render one caption would make the caption fail when the
/// row is deleted mid-window. The window is a record of what was decided at
/// 22:00; it does not become untrue because the schedule was renamed at
/// midnight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunWindow {
    pub schedule_id: String,
    pub schedule_name: String,
    /// The instant the window was opened — the fire, not the occurrence. A late
    /// fire opens its window now and still closes it at the occurrence's own
    /// stop time.
    pub opened_at: DateTime<Utc>,
    /// When new starts stop. `None` for a schedule with no stop time, which runs
    /// until something pauses it.
    pub closes_at: Option<DateTime<Utc>>,
    /// The schedule's own mode, which overrides the `schedule_mode` default
    /// while this window is open. See this module's header.
    pub mode: ScheduleMode,
    /// The schedule's own limit. Clamped on read by
    /// [`capacity`](crate::scheduler::capacity), never here, so there is one
    /// place a number is held to a range.
    pub max_concurrency: i64,
}

impl RunWindow {
    /// The window `schedule` opens when it fires at `opened_at`.
    pub fn opened_by(
        schedule: &Schedule,
        opened_at: DateTime<Utc>,
        closes_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            schedule_id: schedule.id.clone(),
            schedule_name: schedule.name.clone(),
            opened_at,
            closes_at,
            mode: schedule.mode,
            max_concurrency: schedule.max_concurrency,
        }
    }

    /// Whether `now` is at or past this window's stop time.
    pub fn has_closed(&self, now: DateTime<Utc>) -> bool {
        self.closes_at.is_some_and(|at| now >= at)
    }
}

/// The window that is open, or `None`.
///
/// Tolerant, the way every `settings` read in this codebase is and for
/// ADR-0003's reason: an unreadable value is read as "no window", which costs a
/// night that runs under the default configuration rather than a queue that
/// refuses to run at all. Note the direction — falling back to *no* window is
/// the narrow reading, because a window is what *raises* concurrency above the
/// default.
pub async fn active(pool: &SqlitePool) -> Result<Option<RunWindow>> {
    let Some(stored) = settings::get(pool, ACTIVE_RUN_WINDOW).await? else {
        return Ok(None);
    };
    if stored.trim().is_empty() {
        return Ok(None);
    }

    match serde_json::from_str::<RunWindow>(&stored) {
        Ok(window) => Ok(Some(window)),
        Err(error) => {
            tracing::warn!(
                %error,
                value = stored,
                "unreadable active_run_window; treating the queue as having no window open",
            );
            Ok(None)
        }
    }
}

/// Records that a window is open, replacing any that was.
///
/// Replacing rather than refusing is deliberate and is not a race the caller
/// has to guard: `tick_schedules` never reaches here with a window already open
/// — it declines to reopen and says so — so the only way to overwrite one is a
/// caller that meant to.
pub async fn open(ctx: &ServiceContext, window: &RunWindow) -> Result<()> {
    let encoded = serde_json::to_string(window).map_err(|error| {
        crate::error::Error::internal(format!("could not record the run window: {error}"))
    })?;
    settings::set(ctx, ACTIVE_RUN_WINDOW, &encoded).await
}

/// Closes whatever window was open.
///
/// Idempotent, and cheap when there is nothing to close: closing is called from
/// `pause`, from `stop`, from the exit path and from the stop time arriving, and
/// three of those four routinely run with no window open at all.
pub async fn close(ctx: &ServiceContext) -> Result<()> {
    if settings::get(&ctx.pool, ACTIVE_RUN_WINDOW).await?.is_none() {
        return Ok(());
    }
    settings::set(ctx, ACTIVE_RUN_WINDOW, "").await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{test_pool, TestContext};
    use pretty_assertions::assert_eq;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    fn window() -> RunWindow {
        RunWindow {
            schedule_id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            schedule_name: "Nightly".to_string(),
            opened_at: at("2026-01-15T21:00:00Z"),
            closes_at: Some(at("2026-01-16T05:00:00Z")),
            mode: ScheduleMode::Parallel,
            max_concurrency: 3,
        }
    }

    #[tokio::test]
    async fn a_queue_nobody_has_scheduled_has_no_window() {
        let pool = test_pool().await;

        assert_eq!(active(&pool).await.expect("read the key"), None);
    }

    #[tokio::test]
    async fn a_window_survives_being_read_back_through_the_row_it_wrote() {
        // The whole reason it is a row rather than a field: a relaunch at 03:00
        // must still know the window closes at 06:00.
        let harness = TestContext::new().await;

        open(&harness.context, &window())
            .await
            .expect("open a window");

        assert_eq!(
            active(&harness.context.pool).await.expect("read it back"),
            Some(window()),
        );
    }

    #[tokio::test]
    async fn closing_leaves_no_window_and_is_safe_to_repeat() {
        let harness = TestContext::new().await;
        open(&harness.context, &window())
            .await
            .expect("open a window");

        close(&harness.context).await.expect("close it");
        close(&harness.context).await.expect("and again");

        assert_eq!(active(&harness.context.pool).await.expect("read"), None);
    }

    #[tokio::test]
    async fn closing_a_queue_that_never_had_a_window_writes_nothing_at_all() {
        // Called from `pause`, from `stop` and from the exit path, three of
        // which routinely run with no window open. A write there would publish
        // a `settings:changed` on every Pause for no reason.
        let harness = TestContext::new().await;

        close(&harness.context).await.expect("close nothing");

        assert_eq!(
            settings::get(&harness.context.pool, ACTIVE_RUN_WINDOW)
                .await
                .expect("read the key"),
            None,
        );
    }

    #[tokio::test]
    async fn a_hand_edited_window_is_read_as_no_window_rather_than_failing_the_night() {
        let harness = TestContext::new().await;

        for nonsense in ["{", "null", "\"nightly\"", "{\"scheduleId\":\"x\"}"] {
            settings::set(&harness.context, ACTIVE_RUN_WINDOW, nonsense)
                .await
                .expect("store nonsense");
            assert_eq!(
                active(&harness.context.pool)
                    .await
                    .expect("a bad row is not an error"),
                None,
                "{nonsense:?}",
            );
        }
    }

    #[test]
    fn a_window_closes_at_its_stop_time_and_not_before() {
        let window = window();

        assert!(!window.has_closed(at("2026-01-16T04:59:59Z")));
        assert!(window.has_closed(at("2026-01-16T05:00:00Z")));
        assert!(window.has_closed(at("2026-01-16T09:00:00Z")));
    }

    #[test]
    fn a_window_with_no_stop_time_never_closes_on_its_own() {
        let endless = RunWindow {
            closes_at: None,
            ..window()
        };

        assert!(!endless.has_closed(at("2027-01-01T00:00:00Z")));
    }
}
