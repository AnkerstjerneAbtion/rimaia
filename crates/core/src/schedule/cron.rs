//! The only file that knows `croner` and `chrono-tz` exist.
//!
//! Everything else in this module tree — [`fire`](super::fire), the service,
//! the window — speaks [`DateTime<Utc>`] and an IANA name, and asks here when it
//! needs one turned into the other. Seam-contract D24 records the confinement as
//! a rule rather than a habit: croner's own README calls its API "subject to
//! change" and 4.0 is recent, so the whole cost of replacing it is this file and
//! its tests. A `Cron` value leaking into a service signature would make that a
//! rewrite instead.
//!
//! # Why croner rather than the alternatives
//!
//! ADR-0010's acceptance criterion is a nightly schedule showing the correct
//! next fire time *across a DST boundary in the configured timezone*, which is a
//! question a cron library either answers or does not. `cron 0.17` leaves its
//! behaviour inside a skipped hour unspecified, so the criterion would be
//! untestable against it — not failing, unanswerable. `saffron` hands the
//! timezone back to the caller, which task 013's Notes explicitly forbid
//! ("hand-rolled cron parsing is a reliable source of 'why didn't it run last
//! night'"). croner searches along the **absolute** time line rather than the
//! wall-clock one, which is what makes a search that starts before a transition
//! and ends after it arrive at a real instant.
//!
//! # What croner does *not* answer, and this file does
//!
//! croner resolves the cron path. It knows nothing about the two other local
//! times a schedule carries: a one-shot `start_at` and the `stop_at` time of
//! day. Both are wall-clock civil times that have to be resolved through the
//! schedule's own zone, and [`chrono::TimeZone::from_local_datetime`] answers
//! that with a three-armed [`LocalResult`], every arm of which is reachable
//! twice a year:
//!
//! - **`Single`** — the ordinary day.
//! - **`None`** — the hour spring-forward skipped. "Stop at 02:30" on the last
//!   Sunday of March in Europe/Copenhagen names a time that does not exist.
//!   [`resolve_local`] takes **the first valid instant after the gap**, which is
//!   the transition itself: the alternative readings are to skip the occurrence
//!   (a night that silently does not run) or to fail (a queue that refuses over
//!   a calendar).
//! - **`Ambiguous`** — the hour autumn-fallback repeats. [`resolve_local`] takes
//!   **the earlier** of the two. Taking the later would run the window an hour
//!   short on exactly the night it is an hour longer, and "the first time the
//!   clock said 02:30" is what a person means by it.
//!
//! Never skip, never double. Those two sentences are the whole DST contract, and
//! `a_gap_resolves_forward_to_the_first_instant_that_exists` and
//! `an_ambiguous_local_time_resolves_to_the_earlier_of_the_two` are what hold
//! them.

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use croner::Cron;
use std::str::FromStr;

use crate::error::{Error, Result};

/// How far [`resolve_local`] will walk out of a gap before giving up.
///
/// Four hours is a wide margin over the widest transition any zone has ever
/// applied (two hours, in a handful of historical cases; the common shift is
/// one, and Lord Howe's is thirty minutes). It is a bound on a loop rather than
/// a tuned value: the walk exists so the gap arm needs no reasoning about
/// offsets, and a bound that can never be reached is cheaper to read than one
/// that is exactly right.
const WIDEST_GAP_MINUTES: i64 = 240;

/// Turns an IANA name into a zone, refusing anything that is not one.
///
/// **Strict, unlike every `settings` read in this codebase**, and the asymmetry
/// is deliberate rather than an oversight. The tolerant rule
/// [`QueueState::from_stored`](crate::scheduler::QueueState) and its siblings
/// follow is right for a key whose fallback is *safe*: an unreadable
/// `max_concurrency` falls back to running fewer things. There is no safe
/// fallback for a zone. Silently reading an unknown name as UTC is how a nightly
/// queue runs at 23:00 in January and 22:00 in June without anybody being told,
/// which is precisely the failure the migration's own comment says the nullable
/// column exists to catch.
pub fn zone(name: &str) -> Result<Tz> {
    Tz::from_str(name.trim()).map_err(|_| {
        Error::invalid(format!(
            "{name:?} is not an IANA timezone name. Pick one from the list, such as \
             \"Europe/Copenhagen\" or \"UTC\"."
        ))
    })
}

/// Every zone the picker offers, in the order `chrono-tz` lists them.
///
/// The reason the frontend needs no npm timezone package: the list the `<select>`
/// is filled from and the list [`zone`] will accept are the same list, generated
/// from the same table, so a name the user can pick is a name the service can
/// store by construction.
pub fn zone_names() -> Vec<String> {
    chrono_tz::TZ_VARIANTS
        .iter()
        .map(|zone| zone.name().to_string())
        .collect()
}

/// Parses a cron expression, refusing one that will never fire.
///
/// Kept separate from the searches so a form can validate an expression at the
/// moment it is typed — task 013's whole point is that "a wrong cron expression
/// is caught in the evening rather than discovered in the morning", and a
/// service that only found out at 22:00 would be too late by the length of a
/// night.
pub fn check(expression: &str) -> Result<()> {
    parse(expression).map(|_| ())
}

fn parse(expression: &str) -> Result<Cron> {
    Cron::from_str(expression.trim()).map_err(|error| {
        Error::invalid(format!(
            "{expression:?} is not a cron expression Rimaia can read: {error}. \
             A nightly queue at 22:00 is \"0 22 * * *\"."
        ))
    })
}

/// The first occurrence of `expression` strictly after `after`, in `zone`.
///
/// The search runs on a [`DateTime<Tz>`], never on a naive time, which is what
/// hands croner the transition table it needs — its searches "move in real time,
/// not wall clock time", so an expression that names 02:30 on a night when 02:30
/// does not exist still lands on a real instant rather than on a value that
/// cannot be converted.
pub fn next_after(expression: &str, zone: Tz, after: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let cron = parse(expression)?;
    let from = after.with_timezone(&zone);
    cron.find_next_occurrence(&from, false)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|error| {
            Error::invalid(format!(
                "{expression:?} has no next occurrence after {}: {error}",
                after.to_rfc3339(),
            ))
        })
}

/// The most recent occurrence of `expression` at or before `at`, in `zone`.
///
/// **This is what makes late firing coalesce**, and it is the reason the
/// backwards search exists at all. A machine asleep from Friday to Monday missed
/// three occurrences of a nightly schedule; asking "what is the latest one that
/// has already happened" answers with one instant, so one fire happens, rather
/// than the three a forward walk from the baseline would enumerate. See
/// [`fire::due_at`](super::fire::due_at), which compares the answer against
/// `max(last_fired_at, armed_at)` and fires only when it is newer than both.
pub fn latest_before(expression: &str, zone: Tz, at: DateTime<Utc>) -> Result<DateTime<Utc>> {
    let cron = parse(expression)?;
    let from = at.with_timezone(&zone);
    cron.find_previous_occurrence(&from, true)
        .map(|at| at.with_timezone(&Utc))
        .map_err(|error| {
            Error::invalid(format!(
                "{expression:?} has no occurrence at or before {}: {error}",
                at.to_rfc3339(),
            ))
        })
}

/// A civil date and time in `zone`, as an instant — resolving both DST arms.
///
/// A gap takes the first valid instant after it; an ambiguity takes the earlier.
/// See this module's header for why those two directions and not the others.
pub fn resolve_local(zone: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    match zone.from_local_datetime(&local) {
        LocalResult::Single(at) => at.with_timezone(&Utc),
        // The earlier of the two, which is the first time the wall clock read
        // this. `earlier` is the one with the *larger* offset — before the clocks
        // went back — and chrono hands them to us in that order already.
        LocalResult::Ambiguous(earlier, _later) => earlier.with_timezone(&Utc),
        // The hour that does not exist. Walking forward a minute at a time
        // lands on the first civil minute that does exist, whose instant is the
        // transition itself. Deliberately a walk and not offset arithmetic:
        // every input that reaches here is minute-aligned (a cron minute, or an
        // `HH:MM` stop time) and so is every real transition, so the walk is
        // exact — and it needs no argument about which side's offset applies,
        // which is where a clever version of this would be wrong twice a year.
        LocalResult::None => first_instant_after_the_gap(zone, local),
    }
}

fn first_instant_after_the_gap(zone: Tz, local: NaiveDateTime) -> DateTime<Utc> {
    for minute in 1..=WIDEST_GAP_MINUTES {
        let candidate = local + Duration::minutes(minute);
        match zone.from_local_datetime(&candidate) {
            LocalResult::Single(at) => return at.with_timezone(&Utc),
            LocalResult::Ambiguous(earlier, _) => return earlier.with_timezone(&Utc),
            LocalResult::None => continue,
        }
    }

    // Unreachable against the real transition table — no zone has ever skipped
    // four hours. Falling back to the naive time read as UTC rather than
    // panicking, because a schedule is not worth taking the process down for,
    // and a zone this exotic would produce a wrong fire time rather than a
    // corrupt one.
    tracing::error!(
        zone = %zone.name(),
        local = %local,
        "no valid instant within four hours of a local time; reading it as UTC",
    );
    Utc.from_utc_datetime(&local)
}

/// `HH:MM` as a civil time of day.
///
/// The stored spelling of `schedules.stop_at`. Parsed here rather than in the
/// service because this is the file that owns turning civil times into instants,
/// and a stop time parsed in one place and resolved in another is two chances to
/// disagree about what "06:00" is.
pub fn time_of_day(value: &str) -> Result<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M").map_err(|_| {
        Error::invalid(format!(
            "{value:?} is not a time of day. Write it as HH:MM, such as \"06:00\"."
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use pretty_assertions::assert_eq;

    /// Europe/Copenhagen, because it is the operator's zone and because its
    /// transitions are the ordinary one-hour European ones — the case a nightly
    /// queue actually meets.
    const CPH: Tz = chrono_tz::Europe::Copenhagen;

    /// Nightly at 22:00, which is the schedule this whole task is named for.
    const NIGHTLY: &str = "0 22 * * *";

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    fn local(date: (i32, u32, u32), time: (u32, u32)) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(date.0, date.1, date.2)
            .expect("a literal date")
            .and_hms_opt(time.0, time.1, 0)
            .expect("a literal time")
    }

    // -----------------------------------------------------------------------
    // Ordinary searching
    // -----------------------------------------------------------------------

    #[test]
    fn a_nightly_schedule_fires_at_its_local_hour_whatever_the_offset_is() {
        // Winter is UTC+1 and summer is UTC+2, so the same expression is two
        // different UTC instants — which is the entire reason the column stores
        // a zone rather than an offset.
        assert_eq!(
            next_after(NIGHTLY, CPH, at("2026-01-15T12:00:00Z")).expect("a next occurrence"),
            at("2026-01-15T21:00:00Z"),
        );
        assert_eq!(
            next_after(NIGHTLY, CPH, at("2026-07-15T12:00:00Z")).expect("a next occurrence"),
            at("2026-07-15T20:00:00Z"),
        );
    }

    #[test]
    fn the_search_is_exclusive_of_the_instant_it_starts_from() {
        // Load-bearing for the loop: `next_fire_at` is asked immediately after a
        // fire, and an inclusive search would hand back the occurrence that just
        // fired and read as permanently overdue.
        let fired = at("2026-01-15T21:00:00Z");
        assert_eq!(
            next_after(NIGHTLY, CPH, fired).expect("a next occurrence"),
            at("2026-01-16T21:00:00Z"),
        );
    }

    #[test]
    fn the_backwards_search_is_inclusive_so_an_exact_hit_is_already_due() {
        // The other direction, and the opposite convention on purpose: a loop
        // that wakes exactly at 22:00:00 must find 22:00 rather than yesterday's.
        let exactly = at("2026-01-15T21:00:00Z");
        assert_eq!(
            latest_before(NIGHTLY, CPH, exactly).expect("a previous occurrence"),
            exactly,
        );
    }

    #[test]
    fn the_backwards_search_answers_with_one_instant_however_many_were_missed() {
        // Coalescing, at its source. Three nights asleep is still one answer,
        // and it is the newest — which is what stops a Monday launch opening a
        // window whose stop time was Saturday morning.
        assert_eq!(
            latest_before(NIGHTLY, CPH, at("2026-01-18T09:00:00Z")).expect("a previous occurrence"),
            at("2026-01-17T21:00:00Z"),
        );
    }

    // -----------------------------------------------------------------------
    // DST: the acceptance criterion, in both directions
    // -----------------------------------------------------------------------

    #[test]
    fn a_nightly_schedule_crosses_spring_forward_without_skipping_a_night() {
        // Europe/Copenhagen springs forward at 02:00 local on 2026-03-29.
        // 22:00 on the 28th is UTC+1; 22:00 on the 29th is UTC+2. The night in
        // between is one real hour shorter and the schedule still fires once on
        // each side of it.
        let before = next_after(NIGHTLY, CPH, at("2026-03-28T12:00:00Z")).expect("the 28th");
        assert_eq!(before, at("2026-03-28T21:00:00Z"));

        let after = next_after(NIGHTLY, CPH, before).expect("the 29th");
        assert_eq!(
            after,
            at("2026-03-29T20:00:00Z"),
            "the offset moved, so the same local hour is an hour earlier in UTC",
        );
        assert_eq!(
            after - before,
            Duration::hours(23),
            "the day the clocks went forward is twenty-three real hours long",
        );
    }

    #[test]
    fn a_nightly_schedule_crosses_autumn_fallback_without_firing_twice() {
        // Europe/Copenhagen falls back at 03:00 local on 2026-10-25. The day is
        // twenty-five real hours long and 22:00 happens exactly once in it.
        let before = next_after(NIGHTLY, CPH, at("2026-10-24T12:00:00Z")).expect("the 24th");
        assert_eq!(before, at("2026-10-24T20:00:00Z"));

        let after = next_after(NIGHTLY, CPH, before).expect("the 25th");
        assert_eq!(after, at("2026-10-25T21:00:00Z"));
        assert_eq!(
            after - before,
            Duration::hours(25),
            "the day the clocks went back is twenty-five real hours long",
        );

        assert_eq!(
            next_after(NIGHTLY, CPH, after).expect("the 26th"),
            at("2026-10-26T21:00:00Z"),
            "and the night after it is an ordinary twenty-four",
        );
    }

    #[test]
    fn a_schedule_inside_the_repeated_hour_fires_once_not_twice() {
        // 02:30 local happens twice on 2026-10-25. A schedule naming it must
        // fire once — "never double" — and croner's absolute-time search is what
        // makes that true without this module counting anything.
        let expression = "30 2 * * *";
        let first = next_after(expression, CPH, at("2026-10-25T00:00:00Z")).expect("the 25th");
        let next = next_after(expression, CPH, first).expect("the day after");

        assert_eq!(first, at("2026-10-25T00:30:00Z"), "the earlier 02:30, UTC+2");
        assert!(
            next >= at("2026-10-26T00:00:00Z"),
            "{next} is the second half of the repeated hour, not the next day",
        );
    }

    #[test]
    fn a_schedule_inside_the_skipped_hour_still_fires_that_night() {
        // 02:30 local does not exist on 2026-03-29. "Never skip": the night must
        // still produce an occurrence rather than silently having none.
        let expression = "30 2 * * *";
        let fired = next_after(expression, CPH, at("2026-03-29T00:00:00Z")).expect("the 29th");

        assert!(
            fired < at("2026-03-30T00:00:00Z"),
            "{fired} skipped the night the clocks went forward",
        );
    }

    // -----------------------------------------------------------------------
    // resolve_local: the two arms this file owns rather than croner
    // -----------------------------------------------------------------------

    #[test]
    fn an_ordinary_local_time_resolves_to_the_one_instant_it_names() {
        assert_eq!(
            resolve_local(CPH, local((2026, 1, 15), (6, 0))),
            at("2026-01-15T05:00:00Z"),
        );
        assert_eq!(
            resolve_local(CPH, local((2026, 7, 15), (6, 0))),
            at("2026-07-15T04:00:00Z"),
        );
    }

    #[test]
    fn a_gap_resolves_forward_to_the_first_instant_that_exists() {
        // 02:00 -> 03:00 local on 2026-03-29, so 02:30 is not a time. The answer
        // is the transition itself, 01:00 UTC — which is 03:00 local, the first
        // moment the clock reads a time at least as late as the one asked for.
        let resolved = resolve_local(CPH, local((2026, 3, 29), (2, 30)));

        assert_eq!(resolved, at("2026-03-29T01:00:00Z"));
        assert_eq!(
            resolved.with_timezone(&CPH).format("%H:%M").to_string(),
            "03:00",
            "the first valid instant after the gap, never before it",
        );
    }

    #[test]
    fn an_ambiguous_local_time_resolves_to_the_earlier_of_the_two() {
        // 03:00 -> 02:00 local on 2026-10-25, so 02:30 happens twice: once at
        // 00:30 UTC (UTC+2) and again at 01:30 UTC (UTC+1). The earlier one.
        let resolved = resolve_local(CPH, local((2026, 10, 25), (2, 30)));

        assert_eq!(resolved, at("2026-10-25T00:30:00Z"));
        assert!(
            resolved < at("2026-10-25T01:30:00Z"),
            "taking the later one would end a window an hour late on the one \
             night it is already an hour longer",
        );
    }

    #[test]
    fn a_stop_time_on_the_far_side_of_a_gap_still_lands_at_the_hour_it_names() {
        // The case the column comment argues for: a window opened at 22:00 on
        // the 28th and stopping at 06:00 on the 29th is seven real hours rather
        // than eight, and still ends at 06:00 local.
        let opened = resolve_local(CPH, local((2026, 3, 28), (22, 0)));
        let closed = resolve_local(CPH, local((2026, 3, 29), (6, 0)));

        assert_eq!(closed - opened, Duration::hours(7));
        assert_eq!(
            closed.with_timezone(&CPH).format("%H:%M").to_string(),
            "06:00",
        );
    }

    // -----------------------------------------------------------------------
    // Refusals
    // -----------------------------------------------------------------------

    #[test]
    fn a_zone_that_is_not_an_iana_name_is_refused_rather_than_read_as_utc() {
        // The asymmetry this file's header argues for: there is no safe fallback
        // for a zone, so this one read is strict where every `settings` read is
        // tolerant.
        for name in ["", "Europe/Copenhagn", "CEST", "+01:00"] {
            let error = zone(name).expect_err("{name} must not parse");
            assert!(
                error.to_string().contains("IANA"),
                "the refusal has to say what kind of name it wanted: {error}",
            );
        }
    }

    #[test]
    fn every_name_the_picker_offers_is_a_name_the_service_accepts() {
        // The property that removes the npm timezone package: one table feeds
        // both ends, so a pickable name is a storable name by construction.
        let names = zone_names();
        assert!(names.len() > 300, "only {} zones", names.len());
        assert!(names.iter().any(|name| name == "Europe/Copenhagen"));
        for name in &names {
            zone(name).unwrap_or_else(|error| panic!("{name} is offered but refused: {error}"));
        }
    }

    #[test]
    fn a_cron_expression_that_will_never_fire_is_refused_when_it_is_typed() {
        for expression in ["", "nightly", "0 22 * *", "99 * * * *"] {
            let error = check(expression).expect_err("{expression} must not parse");
            assert!(
                error.to_string().contains("0 22 * * *"),
                "the refusal has to show a working example: {error}",
            );
        }
        check(NIGHTLY).expect("the nightly expression is legal");
    }

    #[test]
    fn a_time_of_day_is_read_as_hh_mm_and_nothing_else() {
        assert_eq!(
            time_of_day("06:00").expect("a legal stop time"),
            NaiveTime::from_hms_opt(6, 0, 0).expect("a literal time"),
        );
        assert_eq!(
            time_of_day(" 23:45 ").expect("surrounding space is not an error"),
            NaiveTime::from_hms_opt(23, 45, 0).expect("a literal time"),
        );
        for value in ["6am", "0600", "25:00", ""] {
            time_of_day(value).expect_err("{value} must not parse");
        }
    }
}
