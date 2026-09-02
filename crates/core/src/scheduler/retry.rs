//! When a failed attempt is tried again, and when it stops being tried
//! (ADR-0011's retry table).
//!
//! # Entirely pure, and that is the point
//!
//! [`decide`] takes a history, an instant and a seed, and returns a decision.
//! It reads no database, spawns nothing and never asks what time it is — which
//! is what makes ADR-0011's whole table assertable in microseconds, including
//! the fifteen-minute steps. Where the history comes *from* is
//! [`attempts`](super::attempts)'s business; what to do with the answer is
//! [`queue`](super::queue)'s.
//!
//! # The run window is deliberately not a parameter
//!
//! ADR-0011 says a usage-limit wait is "capped by the run window", and this
//! function has no window in it. That is not an oversight: run windows are task
//! 013's, and inventing a second, weaker notion of one here is how the queue
//! would end up with two answers to "may this start at 04:00". When 013 lands it
//! adds a parameter to *this* function — not a second policy beside it — and the
//! decision it returns is clamped in one place.
//!
//! # Jitter is deterministic, and it is not decoration
//!
//! Two tasks that hit one wall at 02:00 read the same `resetsAt` and would
//! otherwise resume in the same instant, stampede the reset, and both be
//! refused. The offset is an FNV-1a hash of the run id rather than a random
//! number: seam-contract D6 forbids a new dependency (`rand` included), a
//! spread that is stable per run is easier to reason about at 2am than one that
//! is not, and a test that had to tolerate randomness would be asserting less
//! than this one does.

use chrono::{DateTime, Duration, Utc};

use crate::db::ExitClass;

/// How many attempts ADR-0011's `transient` row allows before the task is left
/// for a human.
pub const MAX_TRANSIENT_ATTEMPTS: u32 = 5;

/// ADR-0011's "1m, 5m, 15m", in seconds.
pub const TRANSIENT_BACKOFF_STEPS: [i64; 3] = [60, 300, 900];

/// ADR-0011's "15m…" — what every step after the third waits.
pub const TRANSIENT_BACKOFF_TAIL: i64 = 900;

/// ADR-0011's fixed poll for a limit that reported no reset time, in seconds.
pub const USAGE_LIMIT_FALLBACK_POLL: i64 = 900;

/// The widest offset [`jitter`] adds to a usage-limit resume, in seconds.
///
/// A minute rather than an hour: the point is to stop two tasks resuming in the
/// same instant, not to postpone the night. A reset that has genuinely passed
/// is worth acting on promptly.
pub const USAGE_LIMIT_MAX_JITTER: i64 = 60;

/// What a task's current session has already spent, and how its latest attempt
/// ended.
///
/// **Derived from the `runs` rows, never stored.** There is no attempt-count
/// column and there must not be one — see [`attempts::history`](super::attempts::history)
/// for why `session_id` rather than the task is the boundary of a budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptHistory {
    /// How the attempt that just ended ended.
    pub exit_class: ExitClass,
    /// The session every attempt in this budget shares, and the one a resume
    /// would continue.
    pub session_id: String,
    /// Every attempt of this session, including the one that just ended.
    pub attempts_in_session: u32,
    /// How many of them ended [`ExitClass::Transient`].
    pub transient_attempts: u32,
    /// How many of them ended [`ExitClass::Interrupted`].
    pub interrupted_attempts: u32,
    /// What the CLI said about the window, when the latest attempt hit one.
    /// `None` is ADR-0011's "no reset time reported" fallback path, not "no
    /// limit".
    pub usage_limit_resets_at: Option<DateTime<Utc>>,
}

/// Which of ADR-0011's rows produced a resume, for the log line and the badge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Waiting out a window that is closed. Unbounded attempts.
    UsageLimit,
    /// Backing off from something that may well work next time.
    Transient,
    /// ADR-0011's "resume once immediately" — the only kind with no wait.
    Interrupted,
}

/// Why a task will not be tried again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveUpReason {
    /// ADR-0011's `fatal` and `cancelled` rows, plus `success`, which is not a
    /// failure at all. Carried rather than collapsed so the sentence a card
    /// shows can name the class.
    NotRetryable(ExitClass),
    /// The `transient` budget is spent. `attempts` is how many were made.
    AttemptsExhausted { attempts: u32 },
}

/// What ADR-0011's table says to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryDecision {
    /// Resume the session at `at`. Already-past instants are legitimate: an
    /// `interrupted` run resumes immediately, and a reset time the CLI reported
    /// in the past is one that has already happened.
    ResumeAt {
        at: DateTime<Utc>,
        kind: RetryKind,
    },
    GiveUp {
        reason: GiveUpReason,
    },
}

impl RetryDecision {
    /// The instant to store in `runs.resume_after`, or `None` for a task that
    /// is done being tried.
    ///
    /// The one place the two shapes are collapsed, so `finish_run`'s caller and
    /// the reconciler cannot disagree about what a `GiveUp` writes.
    pub fn resume_after(self) -> Option<DateTime<Utc>> {
        match self {
            RetryDecision::ResumeAt { at, .. } => Some(at),
            RetryDecision::GiveUp { .. } => None,
        }
    }
}

/// ADR-0011's table, as a function.
///
/// | Class | Behaviour |
/// | --- | --- |
/// | `usage_limit` | reset + jitter, or `now + 15m + jitter` when none was reported. **Unbounded attempts** |
/// | `transient` | 1m → 5m → 15m → 15m…, giving up at [`MAX_TRANSIENT_ATTEMPTS`] |
/// | `interrupted` | immediately, **once**; every later one spends the transient budget |
/// | `fatal` / `cancelled` / `success` | never |
///
/// `seed` is the run id of the attempt that just ended — see this module's
/// header on why the jitter is derived from it rather than drawn.
pub fn decide(history: &AttemptHistory, now: DateTime<Utc>, seed: &str) -> RetryDecision {
    match history.exit_class {
        ExitClass::UsageLimit => {
            // Never earlier than `now`: a reset the CLI reported in the past —
            // a clock skew, or a wall we noticed late — means the window is
            // already open, and scheduling into the past would make the
            // deadline unreadable on a card without making the resume any
            // sooner. The jitter is still added, because two tasks that both
            // read a stale reset would otherwise still collide.
            let reset = history
                .usage_limit_resets_at
                .unwrap_or_else(|| now + Duration::seconds(USAGE_LIMIT_FALLBACK_POLL))
                .max(now);
            RetryDecision::ResumeAt {
                at: reset + jitter(seed, USAGE_LIMIT_MAX_JITTER),
                kind: RetryKind::UsageLimit,
            }
        }

        // ADR-0011: "resume once immediately, then treat as transient". The
        // first interruption of a session is free — a process that died is
        // most likely to succeed by simply being started again — and every
        // one after it queues up behind the same budget a transient failure
        // spends, because a session that keeps dying is not a transient blip.
        ExitClass::Interrupted if history.interrupted_attempts <= 1 => RetryDecision::ResumeAt {
            at: now,
            kind: RetryKind::Interrupted,
        },

        ExitClass::Transient | ExitClass::Interrupted => back_off(history, now),

        // ADR-0011's `fatal` and `cancelled` rows, and `success`, which reaches
        // this only if a caller asks about a run that did not fail.
        class @ (ExitClass::Fatal | ExitClass::Cancelled | ExitClass::Success) => {
            RetryDecision::GiveUp {
                reason: GiveUpReason::NotRetryable(class),
            }
        }
    }
}

/// The `transient` row: 1m, 5m, 15m, 15m…, and a cap.
///
/// The budget is spent by transient endings *and* by every interruption after
/// the first, which is what "then treat as transient" means — otherwise a
/// session that died in the same place forty times would resume forever with no
/// wait at all, which is the runaway ADR-0011's cap exists to stop.
fn back_off(history: &AttemptHistory, now: DateTime<Utc>) -> RetryDecision {
    let spent = history.transient_attempts + history.interrupted_attempts.saturating_sub(1);

    if spent >= MAX_TRANSIENT_ATTEMPTS {
        return RetryDecision::GiveUp {
            reason: GiveUpReason::AttemptsExhausted { attempts: spent },
        };
    }

    // `spent` counts the attempt that just ended, so the first failure waits
    // the first step rather than the second.
    let step = TRANSIENT_BACKOFF_STEPS
        .get(spent.saturating_sub(1) as usize)
        .copied()
        .unwrap_or(TRANSIENT_BACKOFF_TAIL);

    RetryDecision::ResumeAt {
        at: now + Duration::seconds(step),
        kind: RetryKind::Transient,
    }
}

/// A stable offset in `0..=span_seconds`, derived from `seed`.
///
/// FNV-1a over the seed's bytes: a few lines, no dependency (seam-contract D6),
/// and good enough for the one property that matters — that two different run
/// ids land on different offsets. This is a spreading function, not a hash
/// anything depends on for secrecy.
///
/// A non-positive span is zero rather than an error: "do not spread these"
/// is a sensible thing for a caller to ask for and not a condition anyone can
/// recover from.
pub fn jitter(seed: &str, span_seconds: i64) -> Duration {
    if span_seconds <= 0 {
        return Duration::zero();
    }

    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in seed.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }

    // Inclusive of both ends, so a span of one second can produce either.
    let span = span_seconds as u64 + 1;
    Duration::seconds((hash % span) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn at(rfc3339: &str) -> DateTime<Utc> {
        rfc3339.parse().expect("a literal timestamp must parse")
    }

    /// A session whose only attempt ended `exit_class`.
    fn first(exit_class: ExitClass) -> AttemptHistory {
        AttemptHistory {
            exit_class,
            session_id: "0b6d3e2e-0000-4000-8000-00000000c0de".to_string(),
            attempts_in_session: 1,
            transient_attempts: u32::from(exit_class == ExitClass::Transient),
            interrupted_attempts: u32::from(exit_class == ExitClass::Interrupted),
            usage_limit_resets_at: None,
        }
    }

    const NOW: &str = "2026-08-20T02:00:00Z";
    const SEED: &str = "3f2b1c00-0000-4000-8000-000000000001";

    // -----------------------------------------------------------------------
    // usage_limit
    // -----------------------------------------------------------------------

    #[test]
    fn a_reported_reset_is_waited_for_plus_a_jitter_that_does_not_move_it_far() {
        let mut history = first(ExitClass::UsageLimit);
        history.usage_limit_resets_at = Some(at("2026-08-20T06:00:00Z"));

        let decision = decide(&history, at(NOW), SEED);

        let RetryDecision::ResumeAt { at: resume, kind } = decision else {
            panic!("a usage limit is always retried: {decision:?}");
        };
        assert_eq!(kind, RetryKind::UsageLimit);
        assert!(
            resume >= at("2026-08-20T06:00:00Z")
                && resume <= at("2026-08-20T06:00:00Z") + Duration::seconds(USAGE_LIMIT_MAX_JITTER),
            "{resume} is not the reported reset plus at most a minute",
        );
    }

    #[test]
    fn usage_limit_without_reset_time_falls_back_to_fixed_poll() {
        // ADR-0011's fallback, and the branch `spike/FINDINGS.md` §4 says we
        // have never seen a real payload for.
        let decision = decide(&first(ExitClass::UsageLimit), at(NOW), SEED);

        let RetryDecision::ResumeAt { at: resume, .. } = decision else {
            panic!("a usage limit is always retried: {decision:?}");
        };
        let poll = at(NOW) + Duration::seconds(USAGE_LIMIT_FALLBACK_POLL);
        assert!(
            resume >= poll && resume <= poll + Duration::seconds(USAGE_LIMIT_MAX_JITTER),
            "{resume} is not fifteen minutes out",
        );
    }

    #[test]
    fn a_usage_limit_is_retried_however_many_walls_the_session_has_already_hit() {
        // "Unbounded attempts" is the row's own word, and it is the difference
        // between a queue that survives a five-hour window and one that gives
        // up inside it.
        let history = AttemptHistory {
            attempts_in_session: 40,
            ..first(ExitClass::UsageLimit)
        };

        assert!(matches!(
            decide(&history, at(NOW), SEED),
            RetryDecision::ResumeAt {
                kind: RetryKind::UsageLimit,
                ..
            }
        ));
    }

    #[test]
    fn a_reset_time_already_in_the_past_resumes_now_rather_than_then() {
        let mut history = first(ExitClass::UsageLimit);
        history.usage_limit_resets_at = Some(at("2026-08-20T01:00:00Z"));

        let RetryDecision::ResumeAt { at: resume, .. } = decide(&history, at(NOW), SEED) else {
            panic!("a usage limit is always retried");
        };

        assert!(resume >= at(NOW), "{resume} is before the clock");
    }

    // -----------------------------------------------------------------------
    // transient
    // -----------------------------------------------------------------------

    #[test]
    fn transient_failures_back_off_one_five_then_fifteen_minutes_and_stay_there() {
        for (attempt, seconds) in [(1, 60), (2, 300), (3, 900), (4, 900)] {
            let history = AttemptHistory {
                attempts_in_session: attempt,
                transient_attempts: attempt,
                ..first(ExitClass::Transient)
            };

            assert_eq!(
                decide(&history, at(NOW), SEED),
                RetryDecision::ResumeAt {
                    at: at(NOW) + Duration::seconds(seconds),
                    kind: RetryKind::Transient,
                },
                "attempt {attempt}",
            );
        }
    }

    #[test]
    fn a_fifteen_minute_backoff_is_decided_without_the_clock_moving() {
        // Task 014's acceptance criterion, at its narrowest: the fifteen-minute
        // step is a value, not an elapsed interval, so deciding it costs
        // nothing. The loop's half of the same promise is `TestClock`'s
        // `advancing_the_clock_resolves_a_wait_the_advance_reached`.
        let history = AttemptHistory {
            attempts_in_session: 3,
            transient_attempts: 3,
            ..first(ExitClass::Transient)
        };

        let decided_at = at(NOW);
        assert_eq!(
            decide(&history, decided_at, SEED),
            RetryDecision::ResumeAt {
                at: decided_at + Duration::minutes(15),
                kind: RetryKind::Transient,
            },
        );
    }

    #[test]
    fn transient_retries_stop_at_the_cap() {
        let history = AttemptHistory {
            attempts_in_session: MAX_TRANSIENT_ATTEMPTS,
            transient_attempts: MAX_TRANSIENT_ATTEMPTS,
            ..first(ExitClass::Transient)
        };

        assert_eq!(
            decide(&history, at(NOW), SEED),
            RetryDecision::GiveUp {
                reason: GiveUpReason::AttemptsExhausted {
                    attempts: MAX_TRANSIENT_ATTEMPTS
                },
            },
        );
    }

    // -----------------------------------------------------------------------
    // interrupted
    // -----------------------------------------------------------------------

    #[test]
    fn an_interrupted_run_resumes_once_immediately_and_then_backs_off_like_a_transient_one() {
        assert_eq!(
            decide(&first(ExitClass::Interrupted), at(NOW), SEED),
            RetryDecision::ResumeAt {
                at: at(NOW),
                kind: RetryKind::Interrupted,
            },
        );

        // The second one is not free. ADR-0011's "then treat as transient".
        let twice = AttemptHistory {
            attempts_in_session: 2,
            interrupted_attempts: 2,
            ..first(ExitClass::Interrupted)
        };
        assert_eq!(
            decide(&twice, at(NOW), SEED),
            RetryDecision::ResumeAt {
                at: at(NOW) + Duration::seconds(60),
                kind: RetryKind::Transient,
            },
        );
    }

    #[test]
    fn interruptions_after_the_first_spend_the_same_budget_transient_failures_do() {
        // The runaway this closes: a session dying in the same place forever
        // would otherwise resume immediately, forever.
        let history = AttemptHistory {
            attempts_in_session: 7,
            transient_attempts: 2,
            interrupted_attempts: 5,
            ..first(ExitClass::Interrupted)
        };

        assert_eq!(
            decide(&history, at(NOW), SEED),
            RetryDecision::GiveUp {
                reason: GiveUpReason::AttemptsExhausted { attempts: 6 },
            },
        );
    }

    // -----------------------------------------------------------------------
    // fatal, cancelled, success
    // -----------------------------------------------------------------------

    #[test]
    fn a_fatal_run_is_not_retried() {
        // ADR-0011's fatal row: bad auth and a missing binary do not get better
        // by being tried again at 03:00.
        for class in [ExitClass::Fatal, ExitClass::Cancelled, ExitClass::Success] {
            assert_eq!(
                decide(&first(class), at(NOW), SEED),
                RetryDecision::GiveUp {
                    reason: GiveUpReason::NotRetryable(class),
                },
                "{class:?}",
            );
        }
    }

    #[test]
    fn a_decision_not_to_retry_stores_no_deadline() {
        assert_eq!(
            decide(&first(ExitClass::Fatal), at(NOW), SEED).resume_after(),
            None
        );
        assert!(decide(&first(ExitClass::UsageLimit), at(NOW), SEED)
            .resume_after()
            .is_some());
    }

    // -----------------------------------------------------------------------
    // jitter
    // -----------------------------------------------------------------------

    #[test]
    fn the_same_run_always_jitters_by_the_same_amount() {
        assert_eq!(
            jitter(SEED, USAGE_LIMIT_MAX_JITTER),
            jitter(SEED, USAGE_LIMIT_MAX_JITTER),
        );
    }

    #[test]
    fn two_tasks_hitting_one_wall_do_not_resume_in_the_same_instant() {
        // The whole reason jitter exists. Asserted over a spread of ids rather
        // than a lucky pair: a hash that mapped everything to zero would pass a
        // two-value test.
        let offsets: std::collections::HashSet<Duration> = (0..64)
            .map(|index| jitter(&format!("run-{index}"), USAGE_LIMIT_MAX_JITTER))
            .collect();

        assert!(
            offsets.len() > 16,
            "64 run ids landed on only {} distinct offsets",
            offsets.len(),
        );
    }

    #[test]
    fn a_jittered_offset_never_leaves_the_span_it_was_given() {
        for index in 0..256 {
            let offset = jitter(&format!("run-{index}"), USAGE_LIMIT_MAX_JITTER);
            assert!(
                offset >= Duration::zero() && offset <= Duration::seconds(USAGE_LIMIT_MAX_JITTER),
                "run-{index} jittered by {offset}",
            );
        }
        assert_eq!(jitter(SEED, 0), Duration::zero());
        assert_eq!(jitter(SEED, -5), Duration::zero());
    }
}
