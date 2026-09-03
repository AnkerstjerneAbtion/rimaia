//! Named schedules: when the queue starts by itself, and until when
//! (task 013; ADR-0010's Triggering and run windows).
//!
//! # The five pieces, and why they are separate
//!
//! [`cron`] is the only file that knows `croner` and `chrono-tz` exist, behind
//! two pure searches and one local-time resolver. [`fire`] is when a schedule is
//! due, entirely pure and clock-injected, so a DST test is a table of values.
//! [`window`] is the one window that is open, stored in `settings` in
//! seam-contract D3's shape. [`preflight`] is what a schedule *would* do, built
//! from [`selection::plan`](crate::scheduler::selection::plan) verbatim. And
//! this file is the CRUD, which is the only part that writes anything.
//!
//! Only the last two touch the database, which is what makes the first three
//! testable as functions — the same split [`scheduler`](crate::scheduler) makes
//! between `selection`/`retry` and `queue`, and for the same reason.
//!
//! # The timer is not here
//!
//! Nothing in this module waits, and nothing in it starts a queue. The wake is a
//! third arm of the queue's own `select!` (see
//! [`queue`](crate::scheduler::queue)'s header), because ADR-0010 makes the
//! scheduler the only component allowed to move a task into `running` and a
//! second task calling `QueueHandle::start` would be a second decider racing
//! `try_step`'s own switch re-checks. This module answers questions; the loop
//! acts on them.
//!
//! # Every row this service writes has a timezone
//!
//! The column is nullable — the migration's own comment explains that a
//! `NOT NULL DEFAULT 'UTC'` would let a nightly schedule be created silently in
//! the wrong zone — and [`validate`] is what makes the nullability harmless: no
//! row this service writes lacks one, and a row that somehow does is refused
//! with a sentence rather than answered about in UTC.

pub mod cron;
pub mod fire;
pub mod preflight;
pub mod window;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::context::ServiceContext;
use crate::db::{new_id, Schedule, ScheduleMode};
use crate::error::{Error, Result};
use crate::events::ChangeEvent;
use crate::scheduler::inflight::CONCURRENCY_CEILING;

pub use fire::{Due, Trigger};
pub use preflight::{preview, PreflightSummary};
pub use window::{RunWindow, ACTIVE_RUN_WINDOW};

/// A schedule, plus the one thing about it that is computed rather than stored.
///
/// The next fire time is on the *view* and not on the row because it is a
/// function of the row and the clock, and a stored copy would be wrong the
/// moment either changed — the same argument [`PreflightSummary`] makes for
/// itself. It is here at all because task 013's Scope requires it to be visible
/// **in the evening**: "so a wrong cron expression is caught in the evening
/// rather than discovered in the morning" is the acceptance criterion, and a
/// list that did not show it would satisfy neither half.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleView {
    #[serde(flatten)]
    pub schedule: Schedule,
    /// When this schedule next fires. `None` for a one-off that already has,
    /// and for a row whose configuration cannot be read at all — in which case
    /// [`next_fire_error`](Self::next_fire_error) says why.
    pub next_fire_at: Option<DateTime<Utc>>,
    /// Why there is no next fire time, when a broken row is the reason.
    ///
    /// **A field rather than a failed read**, and that is the whole point of the
    /// view: one unparseable cron expression must not make the whole list
    /// unreadable, because the list is where the user goes to *fix* it. A
    /// `list_schedules` that returned `Err` for a typo would hide every other
    /// schedule behind the one that is broken.
    pub next_fire_error: Option<String>,
}

impl ScheduleView {
    fn of(schedule: Schedule, now: DateTime<Utc>) -> Self {
        match fire::next_fire_at(&schedule, now) {
            Ok(next_fire_at) => Self {
                schedule,
                next_fire_at,
                next_fire_error: None,
            },
            Err(error) => Self {
                schedule,
                next_fire_at: None,
                next_fire_error: Some(error.to_string()),
            },
        }
    }
}

/// What a form or a tool sends to create or replace a schedule.
///
/// A whole row rather than a patch, unlike
/// [`RepositoryPatch`](crate::repo::RepositoryPatch): a schedule is small,
/// every field is on one form, and the fields constrain each other — `cron` and
/// `start_at` are exclusive, and `stop_at` is meaningless without a `timezone`
/// to resolve it through. A patch would make "clear the cron and set a start
/// time" two writes with an illegal row in between.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduleInput {
    pub name: String,
    pub mode: ScheduleMode,
    pub max_concurrency: i64,
    /// An IANA name. **Required**, though the column is nullable — see this
    /// module's header.
    pub timezone: String,
    /// Exclusive with [`start_at`](Self::start_at).
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default)]
    pub start_at: Option<DateTime<Utc>>,
    /// A local time of day, `HH:MM`.
    #[serde(default)]
    pub stop_at: Option<String>,
    pub enabled: bool,
}

/// Every schedule, with its next fire time, in the order the panel shows them.
///
/// By name, so the list does not reshuffle when a row is edited — the same
/// stability [`repo::list`](crate::repo::list) gives Settings, and for the same
/// reason: a list the user is reading must not move under them.
pub async fn list(ctx: &ServiceContext) -> Result<Vec<ScheduleView>> {
    let now = ctx.clock.now();
    Ok(rows(&ctx.pool)
        .await?
        .into_iter()
        .map(|schedule| ScheduleView::of(schedule, now))
        .collect())
}

/// Every enabled schedule, in a stable order, for the loop to walk.
///
/// **A stable order is load-bearing, not tidy.** Two schedules due in the same
/// minute produce one fire (the loop declines to reopen a window), so *which*
/// one wins has to be the same answer on every pass and after every restart, or
/// a rename would silently change which of two overlapping schedules owns the
/// night.
pub async fn enabled(pool: &SqlitePool) -> Result<Vec<Schedule>> {
    Ok(rows(pool)
        .await?
        .into_iter()
        .filter(|schedule| schedule.enabled)
        .collect())
}

async fn rows(pool: &SqlitePool) -> Result<Vec<Schedule>> {
    let schedules = sqlx::query_as!(
        Schedule,
        r#"
        SELECT id, name, mode AS "mode: ScheduleMode", cron,
               start_at AS "start_at: DateTime<Utc>", max_concurrency,
               enabled AS "enabled: bool", timezone, stop_at,
               last_fired_at AS "last_fired_at: DateTime<Utc>",
               armed_at AS "armed_at: DateTime<Utc>"
        FROM schedules
        ORDER BY name ASC, id ASC
        "#
    )
    .fetch_all(pool)
    .await?;
    Ok(schedules)
}

/// One schedule by id, or `Error::not_found`.
pub async fn get(ctx: &ServiceContext, id: &str) -> Result<Schedule> {
    fetch(&ctx.pool, id).await
}

async fn fetch(pool: &SqlitePool, id: &str) -> Result<Schedule> {
    sqlx::query_as!(
        Schedule,
        r#"
        SELECT id, name, mode AS "mode: ScheduleMode", cron,
               start_at AS "start_at: DateTime<Utc>", max_concurrency,
               enabled AS "enabled: bool", timezone, stop_at,
               last_fired_at AS "last_fired_at: DateTime<Utc>",
               armed_at AS "armed_at: DateTime<Utc>"
        FROM schedules WHERE id = ?1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| Error::not_found(format!("no schedule with id {id}")))
}

/// Creates a schedule, armed from now.
///
/// `armed_at` is set here rather than left NULL because of the failure it
/// closes: a nightly 22:00 schedule created at 23:00 would otherwise fire
/// immediately, for an occurrence an hour older than the row itself. The user
/// who typed "every night at 22:00" at 23:00 meant tomorrow.
pub async fn create(ctx: &ServiceContext, input: ScheduleInput) -> Result<Schedule> {
    let input = validate(input)?;
    let id = new_id();
    let armed_at = ctx.clock.now();
    let mode = input.mode.as_str();

    sqlx::query!(
        r#"
        INSERT INTO schedules
            (id, name, mode, cron, start_at, max_concurrency, enabled, timezone, stop_at, armed_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        "#,
        id,
        input.name,
        mode,
        input.cron,
        input.start_at,
        input.max_concurrency,
        input.enabled,
        input.timezone,
        input.stop_at,
        armed_at,
    )
    .execute(&ctx.pool)
    .await?;

    let schedule = fetch(&ctx.pool, &id).await?;
    ctx.publish(ChangeEvent::schedules([id]));
    Ok(schedule)
}

/// Replaces a schedule's configuration.
///
/// `last_fired_at` is deliberately **left alone**: editing the stop time of a
/// window that is already running tonight must not make tonight's occurrence
/// owed again. `armed_at` is left alone too, unless the edit enables a schedule
/// that was off — which is [`set_enabled`]'s rule, applied here so the two doors
/// cannot disagree about what enabling means.
pub async fn update(ctx: &ServiceContext, id: &str, input: ScheduleInput) -> Result<Schedule> {
    let existing = fetch(&ctx.pool, id).await?;
    let input = validate(input)?;
    let mode = input.mode.as_str();
    // Re-arming on an edit that switches a schedule on, for exactly the reason
    // `set_enabled` re-arms: a month spent disabled is not a month of missed
    // nights to catch up on, and the route the user took to flip the toggle is
    // not something the rule should depend on.
    let armed_at = match (existing.enabled, input.enabled) {
        (false, true) => Some(ctx.clock.now()),
        _ => existing.armed_at,
    };

    sqlx::query!(
        r#"
        UPDATE schedules
           SET name = ?2, mode = ?3, cron = ?4, start_at = ?5, max_concurrency = ?6,
               enabled = ?7, timezone = ?8, stop_at = ?9, armed_at = ?10
         WHERE id = ?1
        "#,
        id,
        input.name,
        mode,
        input.cron,
        input.start_at,
        input.max_concurrency,
        input.enabled,
        input.timezone,
        input.stop_at,
        armed_at,
    )
    .execute(&ctx.pool)
    .await?;

    let schedule = fetch(&ctx.pool, id).await?;
    ctx.publish(ChangeEvent::schedules([id.to_string()]));
    Ok(schedule)
}

/// Turns a schedule on or off without deleting its configuration.
///
/// Task 013's fifth acceptance criterion, and the re-arm is the half that is
/// easy to leave out: switching a schedule back on after a month **moves
/// `armed_at` to now**, so it does not immediately fire for the most recent of
/// thirty missed occurrences. Turning it *off* leaves `armed_at` alone, because
/// there is nothing to protect against — a disabled schedule is not due for
/// anything.
pub async fn set_enabled(ctx: &ServiceContext, id: &str, enabled: bool) -> Result<Schedule> {
    let armed_at = match enabled {
        true => Some(ctx.clock.now()),
        false => fetch(&ctx.pool, id).await?.armed_at,
    };

    let changed = sqlx::query!(
        "UPDATE schedules SET enabled = ?2, armed_at = ?3 WHERE id = ?1",
        id,
        enabled,
        armed_at,
    )
    .execute(&ctx.pool)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(Error::not_found(format!("no schedule with id {id}")));
    }

    let schedule = fetch(&ctx.pool, id).await?;
    ctx.publish(ChangeEvent::schedules([id.to_string()]));
    Ok(schedule)
}

/// Records that a schedule fired, at the instant it **actually** did.
///
/// `now`, never the occurrence: the column's whole purpose is to be the thing a
/// late fire compares against, and storing the occurrence would leave a
/// schedule that fired forty minutes late still reading as owing that
/// occurrence.
///
/// Called by the queue loop and by nothing else. It is the one write in this
/// module that is not a user action, which is why it is not on the command
/// surface at all.
pub async fn record_fire(ctx: &ServiceContext, id: &str, now: DateTime<Utc>) -> Result<()> {
    sqlx::query!(
        "UPDATE schedules SET last_fired_at = ?2 WHERE id = ?1",
        id,
        now,
    )
    .execute(&ctx.pool)
    .await?;
    ctx.publish(ChangeEvent::schedules([id.to_string()]));
    Ok(())
}

/// Deletes a schedule.
///
/// It does **not** close a window this schedule opened, and that is deliberate.
/// A window is a record of a decision that was made at 22:00, not a live
/// reference to a row — [`RunWindow`] carries the name for exactly that reason —
/// so deleting the schedule at midnight leaves tonight running and stops
/// tomorrow. Closing the window is Stop's job, and the user pressing Stop is a
/// different sentence from the user tidying up a list.
pub async fn delete(ctx: &ServiceContext, id: &str) -> Result<()> {
    let changed = sqlx::query!("DELETE FROM schedules WHERE id = ?1", id)
        .execute(&ctx.pool)
        .await?
        .rows_affected();
    if changed == 0 {
        return Err(Error::not_found(format!("no schedule with id {id}")));
    }

    ctx.publish(ChangeEvent::schedules([id.to_string()]));
    Ok(())
}

/// Every IANA zone name, for the picker.
pub fn timezones() -> Vec<String> {
    cron::zone_names()
}

/// Holds an input to everything the schema could not.
///
/// The table carries no `CHECK` for any of this — "which combinations are legal
/// is task 013's design", and SQLite cannot drop a `CHECK` that turns out to be
/// wrong — so these rules live in the service, which is what makes the board and
/// the MCP tool refuse the same rows (ADR-0006).
///
/// Strict, where a *read* of the same row would be tolerant. That asymmetry is
/// the one [`capacity`](crate::scheduler::capacity) already states: a value
/// arriving from a form or a tool is refused with a sentence, while a value
/// already in the file is clamped or fallen back from, because ADR-0003 counts
/// the user as a supported writer of the database and a queue that will not run
/// all night over a typo is the worse outcome.
fn validate(mut input: ScheduleInput) -> Result<ScheduleInput> {
    input.name = input.name.trim().to_string();
    if input.name.is_empty() {
        return Err(Error::invalid("a schedule needs a name"));
    }

    input.timezone = input.timezone.trim().to_string();
    cron::zone(&input.timezone)?;

    input.cron = input
        .cron
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    input.stop_at = input
        .stop_at
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());

    match (input.cron.as_deref(), input.start_at) {
        (Some(expression), None) => cron::check(expression)?,
        (None, Some(_)) => {}
        (Some(_), Some(_)) => {
            return Err(Error::invalid(
                "a schedule repeats or happens once, not both. Clear the time, or clear the \
                 repeating expression.",
            ))
        }
        (None, None) => {
            return Err(Error::invalid(
                "a schedule needs a time: either a one-off moment, or a repeating expression such \
                 as \"0 22 * * *\" for every night at 22:00. To start the queue right now, press \
                 Start.",
            ))
        }
    }

    if let Some(stop_at) = input.stop_at.as_deref() {
        cron::time_of_day(stop_at)?;
    }

    if !(1..=CONCURRENCY_CEILING as i64).contains(&input.max_concurrency) {
        return Err(Error::invalid(format!(
            "a schedule runs between 1 and {CONCURRENCY_CEILING} tasks at once, not {}. \
             To start nothing at all, turn the schedule off.",
            input.max_concurrency,
        )));
    }

    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Clock;
    use crate::testing::TestContext;
    use pretty_assertions::assert_eq;

    const CPH: &str = "Europe/Copenhagen";
    const NIGHTLY: &str = "0 22 * * *";

    fn nightly() -> ScheduleInput {
        ScheduleInput {
            name: "Nightly".to_string(),
            mode: ScheduleMode::Sequential,
            max_concurrency: 2,
            timezone: CPH.to_string(),
            cron: Some(NIGHTLY.to_string()),
            start_at: None,
            stop_at: Some("06:00".to_string()),
            enabled: true,
        }
    }

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    #[tokio::test]
    async fn a_schedule_round_trips_through_the_row_it_wrote() {
        let harness = TestContext::new().await;

        let created = create(&harness.context, nightly())
            .await
            .expect("create a schedule");

        assert_eq!(created.name, "Nightly");
        assert_eq!(created.cron.as_deref(), Some(NIGHTLY));
        assert_eq!(created.timezone.as_deref(), Some(CPH));
        assert_eq!(created.stop_at.as_deref(), Some("06:00"));
        assert_eq!(created.max_concurrency, 2);
        assert!(created.enabled);
        assert_eq!(
            created.armed_at,
            Some(harness.clock.now()),
            "a new schedule is armed from the moment it is created",
        );
        assert_eq!(created.last_fired_at, None);

        assert_eq!(
            get(&harness.context, &created.id).await.expect("read back"),
            created
        );
    }

    #[tokio::test]
    async fn the_list_carries_the_next_fire_time_every_row_is_read_for() {
        // Task 013's Scope: "the next fire time shown for each, so a wrong cron
        // expression is caught in the evening rather than discovered in the
        // morning."
        let harness = TestContext::new().await;
        create(&harness.context, nightly())
            .await
            .expect("create a schedule");

        let listed = list(&harness.context).await.expect("list schedules");

        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].next_fire_at,
            Some(at("2026-08-20T20:00:00Z")),
            "22:00 Copenhagen on the test epoch's own day, in summer time",
        );
        assert_eq!(listed[0].next_fire_error, None);
    }

    #[tokio::test]
    async fn one_broken_row_does_not_make_the_list_unreadable() {
        // The list is where a broken schedule is *fixed*, so it has to render.
        let harness = TestContext::new().await;
        let broken = create(&harness.context, nightly())
            .await
            .expect("create a schedule");
        create(
            &harness.context,
            ScheduleInput {
                name: "Working".to_string(),
                ..nightly()
            },
        )
        .await
        .expect("create a second schedule");
        // Past the service, the way ADR-0003 says the user may.
        sqlx::query!(
            "UPDATE schedules SET cron = 'every night please' WHERE id = ?1",
            broken.id
        )
        .execute(&harness.context.pool)
        .await
        .expect("hand-edit a row");

        let listed = list(&harness.context).await.expect("the list still reads");

        assert_eq!(listed.len(), 2);
        let broken_view = listed
            .iter()
            .find(|view| view.schedule.id == broken.id)
            .expect("the broken row is still listed");
        assert_eq!(broken_view.next_fire_at, None);
        assert!(broken_view
            .next_fire_error
            .as_deref()
            .expect("a reason")
            .contains("0 22 * * *"));
        assert!(listed
            .iter()
            .any(|view| view.schedule.id != broken.id && view.next_fire_at.is_some()));
    }

    #[tokio::test]
    async fn a_schedule_with_no_timezone_is_refused() {
        // The column is nullable and the service is what makes that harmless.
        let harness = TestContext::new().await;

        for timezone in ["", "  ", "CEST", "Europe/Copenhagn"] {
            let error = create(
                &harness.context,
                ScheduleInput {
                    timezone: timezone.to_string(),
                    ..nightly()
                },
            )
            .await
            .expect_err("a schedule without a real zone is not a schedule");
            assert!(error.to_string().contains("IANA"), "{timezone:?}: {error}",);
        }
    }

    #[tokio::test]
    async fn a_schedule_that_is_neither_repeating_nor_one_off_is_refused_and_names_start() {
        // The initial schema's "or neither for run now", declined at the door.
        let harness = TestContext::new().await;

        let error = create(
            &harness.context,
            ScheduleInput {
                cron: None,
                start_at: None,
                ..nightly()
            },
        )
        .await
        .expect_err("run now is not a schedules row");

        assert!(
            error.to_string().contains("press Start"),
            "the refusal has to name the button that already does this: {error}",
        );
    }

    #[tokio::test]
    async fn a_schedule_that_is_both_repeating_and_one_off_is_refused() {
        let harness = TestContext::new().await;

        let error = create(
            &harness.context,
            ScheduleInput {
                start_at: Some(at("2026-08-20T18:30:00Z")),
                ..nightly()
            },
        )
        .await
        .expect_err("a row cannot be both");

        assert!(error.to_string().contains("not both"), "{error}");
    }

    #[tokio::test]
    async fn a_cron_expression_that_will_never_fire_is_refused_when_it_is_saved() {
        let harness = TestContext::new().await;

        let error = create(
            &harness.context,
            ScheduleInput {
                cron: Some("every night".to_string()),
                ..nightly()
            },
        )
        .await
        .expect_err("a typo must not be stored");

        assert!(error.to_string().contains("0 22 * * *"), "{error}");
        assert_eq!(list(&harness.context).await.expect("list").len(), 0);
    }

    #[tokio::test]
    async fn a_stop_time_that_is_not_a_time_of_day_is_refused() {
        let harness = TestContext::new().await;

        let error = create(
            &harness.context,
            ScheduleInput {
                stop_at: Some("6am".to_string()),
                ..nightly()
            },
        )
        .await
        .expect_err("a stop time is HH:MM");

        assert!(error.to_string().contains("HH:MM"), "{error}");
    }

    #[tokio::test]
    async fn a_concurrency_outside_the_range_is_refused_and_names_the_toggle() {
        let harness = TestContext::new().await;

        for refused in [0, -1, CONCURRENCY_CEILING as i64 + 1] {
            let error = create(
                &harness.context,
                ScheduleInput {
                    max_concurrency: refused,
                    ..nightly()
                },
            )
            .await
            .expect_err("a form must not be able to send this");
            assert!(
                error.to_string().contains("turn the schedule off"),
                "{refused}: {error}",
            );
        }
    }

    #[tokio::test]
    async fn a_refused_write_stores_nothing_at_all() {
        let harness = TestContext::new().await;

        create(
            &harness.context,
            ScheduleInput {
                name: "   ".to_string(),
                ..nightly()
            },
        )
        .await
        .expect_err("a blank name is not a name");

        assert_eq!(list(&harness.context).await.expect("list").len(), 0);
    }

    #[tokio::test]
    async fn updating_leaves_the_fire_history_alone() {
        // Editing tonight's stop time must not make tonight's occurrence owed
        // again.
        let harness = TestContext::new().await;
        let created = create(&harness.context, nightly())
            .await
            .expect("create a schedule");
        record_fire(&harness.context, &created.id, harness.clock.now())
            .await
            .expect("fire it");

        let updated = update(
            &harness.context,
            &created.id,
            ScheduleInput {
                stop_at: Some("07:00".to_string()),
                ..nightly()
            },
        )
        .await
        .expect("edit the stop time");

        assert_eq!(updated.stop_at.as_deref(), Some("07:00"));
        assert_eq!(
            updated.last_fired_at,
            Some(harness.clock.now()),
            "an edit is not a reason to re-fire tonight",
        );
        assert_eq!(updated.armed_at, created.armed_at);
    }

    #[tokio::test]
    async fn disabling_keeps_the_configuration_and_enabling_re_arms_it() {
        // Task 013's fifth acceptance criterion, and the re-arm that stops a
        // month of missed nights firing at once.
        let harness = TestContext::new().await;
        let created = create(&harness.context, nightly())
            .await
            .expect("create a schedule");

        let disabled = set_enabled(&harness.context, &created.id, false)
            .await
            .expect("turn it off");
        assert!(!disabled.enabled);
        assert_eq!(
            disabled.cron.as_deref(),
            Some(NIGHTLY),
            "the configuration stays"
        );
        assert_eq!(
            disabled.armed_at, created.armed_at,
            "turning it off arms nothing"
        );
        assert_eq!(
            enabled(&harness.context.pool).await.expect("read").len(),
            0,
            "and the loop stops seeing it",
        );

        harness.clock.advance(chrono::Duration::days(30));
        let re_enabled = set_enabled(&harness.context, &created.id, true)
            .await
            .expect("turn it back on");

        assert!(re_enabled.enabled);
        assert_eq!(
            re_enabled.armed_at,
            Some(harness.clock.now()),
            "a month spent off is not a month of nights to catch up on",
        );
        assert_eq!(enabled(&harness.context.pool).await.expect("read").len(), 1);
    }

    #[tokio::test]
    async fn an_edit_that_switches_a_schedule_on_re_arms_it_exactly_as_the_toggle_does() {
        // Two doors, one rule. The route the user took to enable it is not
        // something the catch-up behaviour should depend on.
        let harness = TestContext::new().await;
        let created = create(
            &harness.context,
            ScheduleInput {
                enabled: false,
                ..nightly()
            },
        )
        .await
        .expect("create a disabled schedule");

        harness.clock.advance(chrono::Duration::days(30));
        let enabled_by_edit = update(&harness.context, &created.id, nightly())
            .await
            .expect("enable it through the form");

        assert_eq!(enabled_by_edit.armed_at, Some(harness.clock.now()));
    }

    #[tokio::test]
    async fn deleting_removes_it_and_deleting_it_twice_says_so() {
        let harness = TestContext::new().await;
        let created = create(&harness.context, nightly())
            .await
            .expect("create a schedule");

        delete(&harness.context, &created.id)
            .await
            .expect("delete it");
        assert_eq!(list(&harness.context).await.expect("list").len(), 0);

        let error = delete(&harness.context, &created.id)
            .await
            .expect_err("there is nothing left to delete");
        assert!(error.to_string().contains("no schedule with id"), "{error}");
    }

    #[tokio::test]
    async fn every_write_announces_itself_on_its_own_channel() {
        // ADR-0018, and seam-contract D24's argument for a variant of its own: a
        // `schedules` row is an entity, not a key/value setting.
        let mut harness = TestContext::new().await;

        let created = create(&harness.context, nightly())
            .await
            .expect("create a schedule");

        assert_eq!(
            harness.changes.try_recv().expect("a waiting publication"),
            ChangeEvent::schedules([created.id.clone()]),
        );
    }

    #[tokio::test]
    async fn the_picker_and_the_service_are_fed_by_one_table() {
        let harness = TestContext::new().await;
        let names = timezones();

        assert!(names.iter().any(|name| name == CPH));
        create(
            &harness.context,
            ScheduleInput {
                timezone: names.last().expect("a zone").clone(),
                ..nightly()
            },
        )
        .await
        .expect("any offered name is a storable name");
    }
}
