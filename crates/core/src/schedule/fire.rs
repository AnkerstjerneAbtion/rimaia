//! When a schedule is due, when its window closes, and why a missed night
//! produces one fire rather than five (ADR-0010's Triggering).
//!
//! # Entirely pure, and that is what makes a DST test cost microseconds
//!
//! Every function here takes a [`Schedule`] and an instant and returns an
//! answer. Nothing reads the database, nothing asks what time it is, and nothing
//! waits — the *loop* is what sleeps, exactly as [`retry::decide`] is pure and
//! `queue` is what acts on it. A nightly schedule crossing spring-forward is
//! therefore a table of values rather than a night of waiting.
//!
//! [`retry::decide`]: crate::scheduler::retry::decide
//!
//! # The four columns, and what each one is for
//!
//! `timezone` is an IANA name, nullable in the schema and **required by the
//! service for every row it writes** — see [`super::create`]. A row without one
//! is a row nothing here can answer about, and it says so rather than guessing
//! UTC.
//!
//! `stop_at` is a **local time of day**, `HH:MM`, never an instant. "Stop at
//! 06:00" is the sentence the user says; an absolute instant cannot express a
//! stop that repeats, and a duration would move the stop whenever the start
//! moved and would end a spring-forward window at the wrong hour. Resolved
//! through the schedule's own zone by [`closes_at`], so a window crossing the
//! gap is seven real hours and still ends at 06:00 local.
//!
//! `last_fired_at` is when the schedule **actually** fired, never when it was
//! due. That distinction is the whole of what makes ADR-0010's "fires late
//! rather than skipping" work without becoming a re-fire loop: the occurrence
//! is in the past, the fire is now, and comparing the next occurrence against
//! *now* is what stops the same missed night firing again a millisecond later.
//!
//! `armed_at` is the instant from which missed occurrences count — set on
//! create, re-set on every enable. Without it, a nightly 22:00 schedule created
//! at 23:00 fires immediately for an occurrence that predates its own
//! existence, and one disabled for a month fires the second it is re-enabled.
//!
//! # Late firing coalesces, and the window's own stop time bounds it
//!
//! [`due`] asks [`cron::latest_before`] for the **most recent** occurrence at or
//! before `now`, not for a walk forward from the baseline. Three nights asleep
//! therefore produce one instant and one fire. Taking the *oldest* missed
//! occurrence instead would be the reading that never runs: its stop time was
//! Saturday morning, so the window it opened would be closed before it opened.
//!
//! And that bound applies to the newest one too. A machine woken at 09:00 on a
//! schedule that runs 22:00–06:00 has genuinely missed the night;
//! [`Due::Expired`] says so rather than opening an eight-hour window that ended
//! three hours ago. ADR-0010's "fires late rather than skipping" is about a
//! machine that was asleep at 22:05, not about one that was asleep until
//! lunchtime.

use chrono::{DateTime, Duration, Utc};

use crate::db::Schedule;
use crate::error::{Error, Result};
use crate::schedule::cron;

/// What a schedule is: one instant, or a repeating expression.
///
/// **There is deliberately no third variant for "run now"**, and the initial
/// schema's own comment expected one — "a cron expression with a timezone, or a
/// wall-clock time, or **neither for run now**". Declining it is
/// seam-contract D24's, and the argument is that
/// [`QueueHandle::start`](crate::scheduler::QueueHandle::start) already *is* Run
/// now: it is the button, it runs the doctor, and it flips the switch. A
/// `schedules` row that nothing ever fires would be a second spelling of that
/// button, with its own enable toggle to leave in the wrong position and its own
/// next-fire time to render as "never". Recorded because the schema expected
/// otherwise, so the absence reads as a decision rather than an omission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// ADR-0010's "Start at" — a wall-clock instant. Fires once.
    Once(DateTime<Utc>),
    /// ADR-0010's "Recurring" — a cron expression, read in the schedule's zone.
    Recurring(String),
}

/// Whether a schedule wants to fire, and what would happen if it did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    /// Fire, for the occurrence at `occurrence`, opening a window that closes at
    /// `closes_at`.
    Fire {
        /// The occurrence being honoured — in the past when the fire is late,
        /// which is the ordinary case after a machine has been asleep.
        occurrence: DateTime<Utc>,
        /// `None` for a window with no stop time, which runs until it is paused.
        closes_at: Option<DateTime<Utc>>,
    },
    /// Due, and too late to matter: the window this occurrence would open has
    /// already ended.
    ///
    /// Deliberately **not** written to `last_fired_at`, which would be a lie
    /// about a fire that did not happen — see [`due`]. The cost is that this
    /// answer is recomputed on each wake until the next occurrence comes round,
    /// which is one cron search against a pure function, and the alternative is
    /// a column that stops meaning what its own migration comment says.
    Expired {
        occurrence: DateTime<Utc>,
        closed_at: DateTime<Utc>,
    },
    /// Nothing to do yet.
    NotDue,
}

impl Due {
    /// The fire, if this is one.
    pub fn fire(&self) -> Option<(DateTime<Utc>, Option<DateTime<Utc>>)> {
        match self {
            Due::Fire {
                occurrence,
                closes_at,
            } => Some((*occurrence, *closes_at)),
            Due::Expired { .. } | Due::NotDue => None,
        }
    }
}

/// What kind of schedule this row is, refusing one that is neither or both.
///
/// The schema deliberately carries no `CHECK` for this ("which combinations are
/// legal is task 013's design"), so the rule lives here, in the service layer
/// ADR-0006 says business rules belong in — which is what makes the board and
/// the MCP tool refuse the same rows.
pub fn trigger(schedule: &Schedule) -> Result<Trigger> {
    match (schedule.cron.as_deref(), schedule.start_at) {
        (Some(expression), None) => Ok(Trigger::Recurring(expression.to_string())),
        (None, Some(at)) => Ok(Trigger::Once(at)),
        (Some(_), Some(_)) => Err(Error::invalid(format!(
            "the schedule {:?} has both a repeating expression and a one-off time. \
             It can have one or the other.",
            schedule.name,
        ))),
        (None, None) => Err(Error::invalid(format!(
            "the schedule {:?} has no time at all. Give it a one-off time, or a repeating \
             expression — a schedule that never fires is not a way to run the queue now; \
             the Start button already is.",
            schedule.name,
        ))),
    }
}

/// The zone this schedule's local times are read in.
///
/// Strict, per [`cron::zone`]: a row whose zone is missing or unreadable cannot
/// be answered about at all, and answering in UTC anyway is how a nightly queue
/// silently runs an hour out for half the year.
fn zone(schedule: &Schedule) -> Result<chrono_tz::Tz> {
    let Some(name) = schedule.timezone.as_deref() else {
        return Err(Error::invalid(format!(
            "the schedule {:?} has no timezone, so Rimaia cannot say when it is due. \
             Set one — a nightly queue without a zone runs an hour out for half the year.",
            schedule.name,
        )));
    };
    cron::zone(name)
}

/// Whether `schedule` should fire at `now`, and what window that would open.
///
/// Disabled schedules are [`Due::NotDue`] without any parsing at all, which is
/// what "disabling a schedule stops it firing **without deleting** its
/// configuration" means operationally: a row with a broken cron expression can
/// still be turned off, and turning it off must not first require fixing it.
pub fn due(schedule: &Schedule, now: DateTime<Utc>) -> Result<Due> {
    if !schedule.enabled {
        return Ok(Due::NotDue);
    }

    let zone = zone(schedule)?;
    let Some(occurrence) = due_at(schedule, zone, now)? else {
        return Ok(Due::NotDue);
    };

    // Computed from the **occurrence**, not from `now`: a window is "22:00 until
    // 06:00", and a fire that is forty minutes late still ends at 06:00 rather
    // than at 06:40. This is also what makes the expired case reachable — a
    // window measured from `now` could never already be over.
    let closes_at = closes_at(schedule, zone, occurrence)?;
    if let Some(closed_at) = closes_at.filter(|at| *at <= now) {
        return Ok(Due::Expired {
            occurrence,
            closed_at,
        });
    }

    Ok(Due::Fire {
        occurrence,
        closes_at,
    })
}

/// The occurrence `schedule` owes at `now`, or `None` when it owes none.
fn due_at(
    schedule: &Schedule,
    zone: chrono_tz::Tz,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    match trigger(schedule)? {
        // A one-shot has fired when it has fired. `last_fired_at` is the whole
        // guard: there is no second occurrence to find, so nothing else could
        // stop it firing again every pass forever.
        Trigger::Once(at) => Ok((schedule.last_fired_at.is_none() && at <= now).then_some(at)),

        Trigger::Recurring(expression) => {
            let latest = cron::latest_before(&expression, zone, now)?;
            // The one comparison the whole "fires late, exactly once" behaviour
            // rests on. `armed_at` keeps a schedule from honouring occurrences
            // that predate its own existence; `last_fired_at` keeps it from
            // honouring one twice. The later of the two is the floor, and a row
            // with neither has no floor — a hand-edited row, and the reading
            // that fires rather than the one that silently never does.
            let baseline = [schedule.last_fired_at, schedule.armed_at]
                .into_iter()
                .flatten()
                .max();
            Ok(match baseline {
                Some(floor) if latest <= floor => None,
                _ => Some(latest),
            })
        }
    }
}

/// When a window opened at `opened_at` stops starting new tasks.
///
/// `stop_at` is a local time of day, so this is "the next time the wall clock in
/// the schedule's own zone reads that". Strictly after the opening: a schedule
/// that starts and stops at the same time means the next one round, not a window
/// of zero length.
pub fn closes_at(
    schedule: &Schedule,
    zone: chrono_tz::Tz,
    opened_at: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>> {
    let Some(stored) = schedule.stop_at.as_deref() else {
        return Ok(None);
    };
    let stop = cron::time_of_day(stored)?;

    // Arithmetic in *local* terms, resolved to an instant only at the end. Doing
    // it the other way round — adding hours to the opening instant — is what
    // makes a spring-forward window end at 05:00 or 07:00 instead of at the
    // hour the user typed.
    let opened_local = opened_at.with_timezone(&zone).naive_local();
    let mut closes_local = opened_local.date().and_time(stop);
    if closes_local <= opened_local {
        closes_local += Duration::days(1);
    }

    Ok(Some(cron::resolve_local(zone, closes_local)))
}

/// The next occurrence **strictly after** `now` — the queue loop's timer.
///
/// A different question from [`next_fire_at`], and the difference is what keeps
/// the loop from spinning. `next_fire_at` answers the *panel*, so an overdue
/// schedule reports the occurrence it owes, which is in the past. A deadline in
/// the past resolves immediately, and a loop that armed one would wake, find the
/// same expired occurrence, arm it again, and burn a core until morning. This
/// one never looks backwards.
///
/// The case that makes it reachable is [`Due::Expired`]: a schedule that owes an
/// occurrence whose window has already closed is not going to fire for it, so
/// the only instant worth waking for is the next one.
pub fn next_wake_at(schedule: &Schedule, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let zone = zone(schedule)?;

    match trigger(schedule)? {
        Trigger::Once(at) => Ok((schedule.last_fired_at.is_none() && at > now).then_some(at)),
        Trigger::Recurring(expression) => cron::next_after(&expression, zone, now).map(Some),
    }
}

/// When this schedule will next fire, for the row the user reads in the evening.
///
/// Answers with an instant **in the past** when the schedule is overdue, rather
/// than with the occurrence after it. A schedule that should have fired an hour
/// ago and has not is exactly the thing task 013 exists to make visible before
/// the night, and rendering tomorrow's time for it would hide the one case worth
/// seeing.
pub fn next_fire_at(schedule: &Schedule, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    let zone = zone(schedule)?;

    match trigger(schedule)? {
        Trigger::Once(at) => Ok(schedule.last_fired_at.is_none().then_some(at)),
        Trigger::Recurring(expression) => match due_at(schedule, zone, now)? {
            Some(overdue) => Ok(Some(overdue)),
            None => cron::next_after(&expression, zone, now).map(Some),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::ScheduleMode;
    use pretty_assertions::assert_eq;

    const NIGHTLY: &str = "0 22 * * *";
    const CPH: &str = "Europe/Copenhagen";

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    /// A nightly 22:00 Europe/Copenhagen schedule stopping at 06:00, armed at
    /// midday on the 15th.
    ///
    /// Armed *between* two occurrences on purpose: a schedule armed a week ago
    /// and never fired legitimately owes the most recent night, which is correct
    /// behaviour and the wrong starting point for a test about whether tonight
    /// is due. Every test below that wants "nothing owed yet" therefore asks
    /// about an instant on the 15th.
    fn nightly() -> Schedule {
        Schedule {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            name: "Nightly".to_string(),
            mode: ScheduleMode::Sequential,
            cron: Some(NIGHTLY.to_string()),
            start_at: None,
            max_concurrency: 2,
            enabled: true,
            timezone: Some(CPH.to_string()),
            stop_at: Some("06:00".to_string()),
            last_fired_at: None,
            armed_at: Some(at("2026-01-15T12:00:00Z")),
        }
    }

    /// The same schedule, armed at `when` — for a test whose subject is an
    /// instant far from the 15th.
    fn armed_at(when: &str) -> Schedule {
        Schedule {
            armed_at: Some(at(when)),
            ..nightly()
        }
    }

    /// A one-shot at `start_at`, otherwise the same.
    fn once(start_at: &str) -> Schedule {
        Schedule {
            cron: None,
            start_at: Some(at(start_at)),
            ..nightly()
        }
    }

    // -----------------------------------------------------------------------
    // What a row is
    // -----------------------------------------------------------------------

    #[test]
    fn a_row_is_a_repeating_expression_or_a_one_off_time_and_never_both() {
        assert_eq!(
            trigger(&nightly()).expect("a cron row"),
            Trigger::Recurring(NIGHTLY.to_string()),
        );
        assert_eq!(
            trigger(&once("2026-01-15T17:30:00Z")).expect("a one-off row"),
            Trigger::Once(at("2026-01-15T17:30:00Z")),
        );

        let both = Schedule {
            start_at: Some(at("2026-01-15T17:30:00Z")),
            ..nightly()
        };
        assert!(trigger(&both)
            .expect_err("both is not a schedule")
            .to_string()
            .contains("one or the other"));
    }

    #[test]
    fn a_row_with_no_time_at_all_is_refused_and_says_the_start_button_is_run_now() {
        // The schema's "or neither for run now", declined. The refusal names the
        // thing the user actually wants, so the absence reads as a decision.
        let neither = Schedule {
            cron: None,
            start_at: None,
            ..nightly()
        };

        let error = trigger(&neither).expect_err("neither is not a schedule");
        assert!(
            error.to_string().contains("Start button already is"),
            "the refusal has to name Run now: {error}",
        );
    }

    #[test]
    fn a_row_without_a_timezone_is_refused_rather_than_read_as_utc() {
        let zoneless = Schedule {
            timezone: None,
            ..nightly()
        };

        let error = due(&zoneless, at("2026-01-15T23:00:00Z")).expect_err("no zone, no answer");
        assert!(
            error.to_string().contains("an hour out"),
            "the refusal has to say what guessing would cost: {error}",
        );
    }

    // -----------------------------------------------------------------------
    // Firing
    // -----------------------------------------------------------------------

    #[test]
    fn a_nightly_schedule_fires_at_its_occurrence_and_not_a_moment_before() {
        let schedule = nightly();

        assert_eq!(
            due(&schedule, at("2026-01-15T20:59:59Z")).expect("read"),
            Due::NotDue,
        );
        assert_eq!(
            due(&schedule, at("2026-01-15T21:00:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-15T21:00:00Z"),
                closes_at: Some(at("2026-01-16T05:00:00Z")),
            },
            "22:00 local in January is 21:00 UTC, and 06:00 local is 05:00",
        );
    }

    #[test]
    fn a_schedule_that_already_fired_this_occurrence_does_not_fire_it_again() {
        // The re-fire loop this column exists to close. `last_fired_at` is when
        // it *actually* fired — three seconds after the occurrence, because the
        // loop woke, ran the doctor and wrote the window.
        let fired = Schedule {
            last_fired_at: Some(at("2026-01-15T21:00:03Z")),
            ..nightly()
        };

        assert_eq!(
            due(&fired, at("2026-01-15T21:00:04Z")).expect("read"),
            Due::NotDue,
        );
        assert_eq!(
            due(&fired, at("2026-01-16T21:00:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-16T21:00:00Z"),
                closes_at: Some(at("2026-01-17T05:00:00Z")),
            },
            "and the next night is a different occurrence, so it fires",
        );
    }

    #[test]
    fn five_missed_occurrences_produce_one_fire_and_it_is_the_newest() {
        // Coalescing. A laptop shut on Sunday evening and opened the following
        // Friday at 22:30 missed four nights; it starts one, tonight's.
        let schedule = nightly();
        let friday_night = at("2026-01-16T21:30:00Z");

        assert_eq!(
            due(&schedule, friday_night).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-16T21:00:00Z"),
                closes_at: Some(at("2026-01-17T05:00:00Z")),
            },
        );
    }

    #[test]
    fn a_window_whose_stop_time_already_passed_does_not_open() {
        // The bound on late firing. Woken at 09:00, the 22:00 occurrence's
        // window ended three hours ago — opening it would start a full night's
        // work in the middle of a working morning.
        let schedule = nightly();

        assert_eq!(
            due(&schedule, at("2026-01-16T08:00:00Z")).expect("read"),
            Due::Expired {
                occurrence: at("2026-01-15T21:00:00Z"),
                closed_at: at("2026-01-16T05:00:00Z"),
            },
        );
    }

    #[test]
    fn a_schedule_with_no_stop_time_fires_however_late_it_is() {
        // The other half: without a stop time there is no window to have missed,
        // and ADR-0010's "fires late rather than skipping" applies unqualified.
        let endless = Schedule {
            stop_at: None,
            ..nightly()
        };

        assert_eq!(
            due(&endless, at("2026-01-16T08:00:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-15T21:00:00Z"),
                closes_at: None,
            },
        );
    }

    #[test]
    fn armed_at_stops_a_schedule_honouring_an_occurrence_older_than_itself() {
        // Created at 23:00 for a nightly 22:00. Without `armed_at` the very next
        // pass fires for an occurrence an hour before the row existed.
        let created_late = Schedule {
            armed_at: Some(at("2026-01-15T22:00:00Z")),
            ..nightly()
        };

        assert_eq!(
            due(&created_late, at("2026-01-15T22:01:00Z")).expect("read"),
            Due::NotDue,
        );
        assert_eq!(
            due(&created_late, at("2026-01-16T21:00:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-16T21:00:00Z"),
                closes_at: Some(at("2026-01-17T05:00:00Z")),
            },
            "the first occurrence after it was armed does fire",
        );
    }

    #[test]
    fn re_arming_stops_a_month_of_disabled_occurrences_firing_at_once() {
        // What re-enabling does. `armed_at` moves to now, so the month the
        // schedule spent disabled is not a month of missed nights to catch up on.
        let re_enabled = Schedule {
            last_fired_at: Some(at("2025-12-15T21:00:03Z")),
            armed_at: Some(at("2026-01-15T14:00:00Z")),
            ..nightly()
        };

        assert_eq!(
            due(&re_enabled, at("2026-01-15T14:00:01Z")).expect("read"),
            Due::NotDue,
            "the baseline is the later of the two, so December is not owed",
        );
    }

    #[test]
    fn a_disabled_schedule_never_fires_and_is_not_even_parsed() {
        // "Disabling stops it firing without deleting its configuration" — and
        // a row that is broken *and* off must be turnable off without first
        // being fixed, which is why the check precedes every parse.
        let off = Schedule {
            enabled: false,
            cron: Some("not a cron expression".to_string()),
            timezone: None,
            ..nightly()
        };

        assert_eq!(
            due(&off, at("2026-01-15T21:00:00Z")).expect("read"),
            Due::NotDue
        );
    }

    // -----------------------------------------------------------------------
    // One-shots
    // -----------------------------------------------------------------------

    #[test]
    fn a_one_off_time_fires_once_and_then_never_again() {
        let schedule = once("2026-01-15T17:30:00Z");

        assert_eq!(
            due(&schedule, at("2026-01-15T17:29:00Z")).expect("read"),
            Due::NotDue,
        );
        assert_eq!(
            due(&schedule, at("2026-01-15T17:30:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-15T17:30:00Z"),
                closes_at: Some(at("2026-01-16T05:00:00Z")),
            },
        );

        let fired = Schedule {
            last_fired_at: Some(at("2026-01-15T17:30:01Z")),
            ..schedule
        };
        assert_eq!(
            due(&fired, at("2026-01-15T18:00:00Z")).expect("read"),
            Due::NotDue,
        );
    }

    #[test]
    fn a_one_off_time_in_the_past_fires_immediately_rather_than_being_skipped() {
        // ADR-0010: "A wall-clock time in the past fires immediately rather than
        // being skipped; the machine having been asleep is the common case."
        let missed = Schedule {
            stop_at: None,
            ..once("2026-01-15T17:30:00Z")
        };

        assert_eq!(
            due(&missed, at("2026-01-15T19:00:00Z")).expect("read"),
            Due::Fire {
                occurrence: at("2026-01-15T17:30:00Z"),
                closes_at: None,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Stop times, and the DST case the column comment argues for
    // -----------------------------------------------------------------------

    #[test]
    fn a_stop_time_earlier_in_the_day_than_the_start_means_the_next_morning() {
        let schedule = nightly();
        let zone = cron::zone(CPH).expect("a literal zone");

        assert_eq!(
            closes_at(&schedule, zone, at("2026-01-15T21:00:00Z")).expect("read"),
            Some(at("2026-01-16T05:00:00Z")),
        );
    }

    #[test]
    fn a_window_crossing_spring_forward_is_seven_real_hours_and_still_ends_at_six() {
        // The sentence the migration's own comment makes, asserted. A duration
        // column would end this window at 05:00 local; an absolute instant could
        // not express "every night at 06:00" at all.
        let schedule = nightly();
        let opened = at("2026-03-28T21:00:00Z");
        let closed = closes_at(&schedule, cron::zone(CPH).expect("a literal zone"), opened)
            .expect("read")
            .expect("a stop time");

        assert_eq!(closed - opened, Duration::hours(7));
        assert_eq!(
            closed
                .with_timezone(&cron::zone(CPH).expect("a literal zone"))
                .format("%H:%M")
                .to_string(),
            "06:00",
        );
    }

    #[test]
    fn a_window_crossing_autumn_fallback_is_nine_real_hours_and_still_ends_at_six() {
        let schedule = nightly();
        let opened = at("2026-10-24T20:00:00Z");
        let closed = closes_at(&schedule, cron::zone(CPH).expect("a literal zone"), opened)
            .expect("read")
            .expect("a stop time");

        assert_eq!(closed - opened, Duration::hours(9));
        assert_eq!(
            closed
                .with_timezone(&cron::zone(CPH).expect("a literal zone"))
                .format("%H:%M")
                .to_string(),
            "06:00",
        );
    }

    // -----------------------------------------------------------------------
    // What the row shows in the evening
    // -----------------------------------------------------------------------

    #[test]
    fn the_next_fire_time_is_the_next_occurrence_when_nothing_is_owed() {
        assert_eq!(
            next_fire_at(&nightly(), at("2026-01-15T12:00:00Z")).expect("read"),
            Some(at("2026-01-15T21:00:00Z")),
        );
    }

    #[test]
    fn the_next_fire_time_of_an_overdue_schedule_is_the_occurrence_it_owes() {
        // The one case worth seeing in the evening: rendering tomorrow's 22:00
        // for a schedule that should have started an hour ago would hide it.
        assert_eq!(
            next_fire_at(&nightly(), at("2026-01-15T23:00:00Z")).expect("read"),
            Some(at("2026-01-15T21:00:00Z")),
        );
    }

    #[test]
    fn the_loops_own_wake_time_never_looks_backwards() {
        // The spin this closes: an overdue schedule's `next_fire_at` is in the
        // past, and a deadline in the past resolves immediately. A loop that
        // armed one would wake, find the same expired occurrence, and arm it
        // again until morning.
        let overdue = at("2026-01-16T08:00:00Z");
        assert_eq!(
            due(&nightly(), overdue).expect("read"),
            Due::Expired {
                occurrence: at("2026-01-15T21:00:00Z"),
                closed_at: at("2026-01-16T05:00:00Z"),
            },
        );

        let wake = next_wake_at(&nightly(), overdue)
            .expect("read")
            .expect("a recurring schedule always has a next one");
        assert!(wake > overdue, "{wake} is not in the future");
        assert_eq!(wake, at("2026-01-16T21:00:00Z"));
    }

    #[test]
    fn a_one_off_the_loop_has_nothing_left_to_wake_for_arms_no_timer() {
        let past = Schedule {
            stop_at: None,
            ..once("2026-01-15T17:30:00Z")
        };

        assert_eq!(
            next_wake_at(&past, at("2026-01-15T19:00:00Z")).expect("read"),
            None,
            "an overdue one-off is due *now*; there is no future instant to wait for",
        );
        assert_eq!(
            next_wake_at(&past, at("2026-01-15T12:00:00Z")).expect("read"),
            Some(at("2026-01-15T17:30:00Z")),
        );
    }

    #[test]
    fn a_one_off_that_has_fired_has_no_next_fire_time_at_all() {
        let fired = Schedule {
            last_fired_at: Some(at("2026-01-15T17:30:01Z")),
            ..once("2026-01-15T17:30:00Z")
        };

        assert_eq!(
            next_fire_at(&fired, at("2026-01-15T18:00:00Z")).expect("read"),
            None,
        );
    }

    #[test]
    fn a_nightly_schedules_next_fire_time_is_right_on_both_sides_of_a_dst_boundary() {
        // Task 013's second acceptance criterion, at the level the panel reads
        // it: the number on the row is correct across the boundary, which is
        // what makes a wrong cron expression visible in the evening.
        //
        // Each is asked at midday about a schedule armed that same midday, so
        // the answer is the night ahead rather than a night already owed.
        for (midday, expected, what) in [
            (
                "2026-03-28T12:00:00Z",
                "2026-03-28T21:00:00Z",
                "the last night of winter time, UTC+1",
            ),
            (
                "2026-03-29T12:00:00Z",
                "2026-03-29T20:00:00Z",
                "the first night of summer time, UTC+2 — the same 22:00 on the wall",
            ),
            (
                "2026-10-25T12:00:00Z",
                "2026-10-25T21:00:00Z",
                "and back again in October",
            ),
        ] {
            assert_eq!(
                next_fire_at(&armed_at(midday), at(midday)).expect("read"),
                Some(at(expected)),
                "{what}",
            );
        }
    }
}
