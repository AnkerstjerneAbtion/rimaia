//! What a closed row means for its task: ADR-0017's exits, as one pure
//! function (task 021).
//!
//! The board calls [`decide`] from `outcome::finish_run`'s task-side step,
//! after the row's `UPDATE` has committed, on every path that closes a row —
//! the runner's finish and reconcile's alike. The runner only reports.
//!
//! | Row that finished | Condition | Next | Task |
//! | --- | --- | --- | --- |
//! | implementation, any failure | — | as before | as before (ADR-0011) |
//! | implementation, success | loop off, or window closed | `Released` | `in_review` |
//! | implementation, success | loop on, window open | `Continue { Review }` | stays `running` |
//! | review, success, recorded, `HEAD` unmoved | no open blocking finding | `Released` | `in_review`, clean |
//! | same | blocking, fixes spent < budget, window open | `Continue { Fix }` | stays `running` |
//! | same | blocking, budget spent or window closed | `Released` | `in_review`, findings remain |
//! | review, success | not recorded, or `HEAD` moved | `Released` | `in_review`, unreviewed |
//! | fix, success | window open | `Continue { Review }` | stays `running` |
//! | fix, success | window closed | `Released` | `in_review`, unreviewed |
//! | review or fix | retryable, `resume_after` set | `Released { resume_after }` | `waiting_retry` |
//! | review or fix | anything else | `Released` | `in_review`, unreviewed |
//!
//! A failed review or fix still lands in `in_review`, because the
//! implementation had already succeeded: losing that to a reviewer's failure
//! would be worse than the loop being off. **No row of the table moves a task
//! to `done`**; a human still approves.

use chrono::{DateTime, Utc};

use crate::board::NextStep;
use crate::db::{BoardColumn, ExitClass, RunKind};
use crate::review::findings::ReviewFinding;

use super::{current_loop, open_blocking, EffectiveReviewConfig, LoopRow};

/// The row that just closed, as far as the decision is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Closed {
    pub kind: RunKind,
    pub exit_class: ExitClass,
    /// What the board's retry policy decided (ADR-0011), already.
    pub resume_after: Option<DateTime<Utc>>,
}

/// Where the task lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Landing {
    /// The loop continues: the task stays in its column, `running`, and
    /// nothing is written to it.
    Stays,
    /// The bottom of `in_review`, `idle`.
    InReview,
    WaitingRetry,
    Failed,
}

impl Landing {
    /// The column the landing moves the task to, if it moves it.
    pub const fn column(self) -> Option<BoardColumn> {
        match self {
            Landing::InReview => Some(BoardColumn::InReview),
            Landing::Stays | Landing::WaitingRetry | Landing::Failed => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    pub next: NextStep,
    pub landing: Landing,
}

impl Decision {
    fn released(landing: Landing, resume_after: Option<DateTime<Utc>>) -> Self {
        Self {
            next: NextStep::Released { resume_after },
            landing,
        }
    }

    fn continuing(kind: RunKind) -> Self {
        Self {
            next: NextStep::Continue { kind },
            landing: Landing::Stays,
        }
    }

    fn in_review() -> Self {
        Self::released(Landing::InReview, None)
    }
}

/// ADR-0017's exits. `rows` is the task's every row, oldest first, the closed
/// one included and already closed; `findings` is the task's every finding.
///
/// `window_closes_at` is the runner's run window (D24 point 4): a loop never
/// starts a phase after it, and `None` is no window. The configuration is the
/// one read at this boundary, so a setting changed mid-loop takes effect here.
pub fn decide(
    config: &EffectiveReviewConfig,
    rows: &[LoopRow],
    findings: &[ReviewFinding],
    closed: Closed,
    window_closes_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Decision {
    let window_open = window_closes_at.is_none_or(|closes| now < closes);
    let may_continue = config.enabled && window_open;

    match (closed.kind, closed.exit_class) {
        (RunKind::Implementation, ExitClass::Success) => {
            if may_continue {
                Decision::continuing(RunKind::Review)
            } else {
                Decision::in_review()
            }
        }
        // ADR-0011's table, as it was before the loop existed.
        (RunKind::Implementation, ExitClass::Fatal | ExitClass::Cancelled) => {
            Decision::released(Landing::Failed, closed.resume_after)
        }
        (RunKind::Implementation, _) => match closed.resume_after {
            Some(at) => Decision::released(Landing::WaitingRetry, Some(at)),
            None => Decision::released(Landing::Failed, None),
        },

        (RunKind::Review, ExitClass::Success) => {
            after_a_review(config, rows, findings, window_open)
        }
        (RunKind::Fix, ExitClass::Success) => {
            if may_continue {
                Decision::continuing(RunKind::Review)
            } else {
                Decision::in_review()
            }
        }
        (
            RunKind::Review | RunKind::Fix,
            ExitClass::UsageLimit | ExitClass::Transient | ExitClass::Interrupted,
        ) if closed.resume_after.is_some() => {
            Decision::released(Landing::WaitingRetry, closed.resume_after)
        }
        (RunKind::Review | RunKind::Fix, _) => Decision::in_review(),
    }
}

/// A review that succeeded: clean, a fix, or the findings attached.
fn after_a_review(
    config: &EffectiveReviewConfig,
    rows: &[LoopRow],
    findings: &[ReviewFinding],
    window_open: bool,
) -> Decision {
    let Some(current) = current_loop(rows) else {
        return Decision::in_review();
    };
    let Some(review) = current
        .phases
        .last()
        .filter(|phase| phase.kind == RunKind::Review)
    else {
        return Decision::in_review();
    };
    // A review that never called, or that changed the branch it was judging,
    // is not a review: unreviewed, and never clean.
    if !review.recorded() || review.moved_head() {
        return Decision::in_review();
    }
    if open_blocking(findings, review, config.blocking_severity).is_empty() {
        return Decision::in_review();
    }
    if config.enabled && window_open && current.fixes_spent() < config.max_review_loops {
        Decision::continuing(RunKind::Fix)
    } else {
        Decision::in_review()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::findings::{FindingSeverity, FindingStatus};
    use crate::review_loop::test_rows::{ended, recorded, row};
    use chrono::Duration;
    use pretty_assertions::assert_eq;

    fn now() -> DateTime<Utc> {
        "2026-08-20T03:00:00Z".parse().expect("a literal timestamp")
    }

    fn on() -> EffectiveReviewConfig {
        EffectiveReviewConfig {
            enabled: true,
            ..EffectiveReviewConfig::default()
        }
    }

    fn finding(review: &LoopRow, severity: FindingSeverity) -> ReviewFinding {
        ReviewFinding {
            id: format!("finding-of-{}", review.id),
            task_id: "task".to_string(),
            review_run_id: review.id.clone(),
            ordinal: 0,
            severity,
            title: "The retry never stops".to_string(),
            body: "The loop has no budget.".to_string(),
            file: Some("src/retry.rs".to_string()),
            line: Some(12),
            fingerprint: Some("src/retry.rs|the retry never stops".to_string()),
            status: FindingStatus::Open,
            resolution: None,
            resolved_by_run_id: None,
            created_at: now(),
            resolved_at: None,
        }
    }

    fn closed(row: &LoopRow) -> Closed {
        Closed {
            kind: row.kind,
            exit_class: row.exit_class.expect("a closed row"),
            resume_after: None,
        }
    }

    fn decide_last(
        config: &EffectiveReviewConfig,
        rows: &[LoopRow],
        findings: &[ReviewFinding],
    ) -> Decision {
        decide(
            config,
            rows,
            findings,
            closed(rows.last().expect("rows")),
            None,
            now(),
        )
    }

    /// An implementation, then a review that recorded `severity` and left
    /// `HEAD` alone.
    fn one_review(severity: FindingSeverity) -> (Vec<LoopRow>, Vec<ReviewFinding>) {
        let rows = vec![
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
        ];
        let findings = vec![finding(&rows[1], severity)];
        (rows, findings)
    }

    #[test]
    fn an_implementation_success_starts_a_review_only_when_the_loop_is_on() {
        let rows = [row(1, RunKind::Implementation, "impl", "a")];
        assert_eq!(
            decide_last(&EffectiveReviewConfig::default(), &rows, &[]),
            Decision::in_review()
        );
        assert_eq!(
            decide_last(&on(), &rows, &[]),
            Decision::continuing(RunKind::Review)
        );
    }

    #[test]
    fn a_clean_review_lands_in_review() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
        ];
        assert_eq!(decide_last(&on(), &rows, &[]), Decision::in_review());
    }

    #[test]
    fn findings_below_the_blocking_severity_do_not_start_a_fix() {
        let (rows, findings) = one_review(FindingSeverity::Low);
        assert_eq!(decide_last(&on(), &rows, &findings), Decision::in_review());

        let (rows, findings) = one_review(FindingSeverity::Medium);
        assert_eq!(
            decide_last(&on(), &rows, &findings),
            Decision::continuing(RunKind::Fix),
            "medium is the default threshold, and meets it",
        );
    }

    #[test]
    fn max_review_loops_zero_reviews_once_and_never_fixes() {
        let config = EffectiveReviewConfig {
            max_review_loops: 0,
            ..on()
        };
        let implementation = [row(1, RunKind::Implementation, "impl", "a")];
        assert_eq!(
            decide_last(&config, &implementation, &[]),
            Decision::continuing(RunKind::Review),
            "report-only mode still reviews",
        );

        let (rows, findings) = one_review(FindingSeverity::Critical);
        assert_eq!(
            decide_last(&config, &rows, &findings),
            Decision::in_review()
        );
    }

    #[test]
    fn a_fix_is_always_followed_by_a_review_while_the_window_is_open() {
        let config = EffectiveReviewConfig {
            max_review_loops: 1,
            ..on()
        };
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
            row(3, RunKind::Fix, "f1", "b"),
        ];
        assert_eq!(
            decide_last(&config, &rows, &[]),
            Decision::continuing(RunKind::Review),
            "the budget is spent, and the fix is still reviewed",
        );
    }

    #[test]
    fn a_closed_run_window_ends_the_loop_at_the_next_phase_boundary() {
        let closes = now() - Duration::minutes(1);
        let open = now() + Duration::minutes(1);
        let (review_rows, findings) = one_review(FindingSeverity::High);
        let cases: [(&[LoopRow], &[ReviewFinding]); 3] = [
            (&review_rows[..1], &[]),
            (&review_rows, &findings),
            (
                &[
                    row(1, RunKind::Implementation, "impl", "a"),
                    recorded(row(2, RunKind::Review, "r1", "a")),
                    row(3, RunKind::Fix, "f1", "b"),
                ],
                &[],
            ),
        ];
        for (rows, findings) in cases {
            let last = closed(rows.last().expect("rows"));
            assert_eq!(
                decide(&on(), rows, findings, last, Some(closes), now()),
                Decision::in_review(),
                "{:?}",
                last.kind
            );
            assert!(matches!(
                decide(&on(), rows, findings, last, Some(open), now()).next,
                NextStep::Continue { .. }
            ));
        }
    }

    #[test]
    fn a_review_that_records_nothing_lands_unreviewed_and_never_clean() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            row(2, RunKind::Review, "r1", "a"),
        ];
        assert_eq!(decide_last(&on(), &rows, &[]), Decision::in_review());
    }

    #[test]
    fn a_review_that_moves_head_lands_unreviewed() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "b")),
        ];
        let findings = [finding(&rows[1], FindingSeverity::High)];
        assert_eq!(
            decide_last(&on(), &rows, &findings),
            Decision::in_review(),
            "a reviewer that committed is an unreviewed fixer, and gets no fix after it",
        );
    }

    #[test]
    fn a_review_that_committed_before_its_usage_limit_still_lands_unreviewed() {
        // Row 2 committed, hit the limit and was resumed as row 3, which
        // committed nothing more: row 3 alone did not move HEAD, the phase did.
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            ended(row(2, RunKind::Review, "r1", "b"), ExitClass::UsageLimit),
            recorded(row(3, RunKind::Review, "r1", "b")),
        ];
        let findings = [finding(&rows[2], FindingSeverity::High)];
        assert_eq!(decide_last(&on(), &rows, &findings), Decision::in_review());
    }

    #[test]
    fn a_resumed_review_that_recorded_before_the_limit_is_not_nothing_recorded() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(ended(
                row(2, RunKind::Review, "r1", "a"),
                ExitClass::UsageLimit,
            )),
            row(3, RunKind::Review, "r1", "a"),
        ];
        let findings = [finding(&rows[1], FindingSeverity::High)];
        assert_eq!(
            decide_last(&on(), &rows, &findings),
            Decision::continuing(RunKind::Fix),
            "the phase recorded, in its first row",
        );
    }

    #[test]
    fn a_fatal_or_cancelled_review_lands_in_review_idle_unreviewed() {
        for exit_class in [ExitClass::Fatal, ExitClass::Cancelled] {
            for kind in [RunKind::Review, RunKind::Fix] {
                let rows = [
                    row(1, RunKind::Implementation, "impl", "a"),
                    ended(row(2, kind, "r1", "a"), exit_class),
                ];
                assert_eq!(
                    decide_last(&on(), &rows, &[]),
                    Decision::in_review(),
                    "{kind:?} {exit_class:?}"
                );
            }
        }
    }

    #[test]
    fn a_retryable_review_with_a_deadline_waits_and_one_without_lands_in_review() {
        let at = now() + Duration::minutes(5);
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            ended(row(2, RunKind::Review, "r1", "a"), ExitClass::Transient),
        ];
        let waiting = Closed {
            resume_after: Some(at),
            ..closed(&rows[1])
        };
        assert_eq!(
            decide(&on(), &rows, &[], waiting, None, now()),
            Decision::released(Landing::WaitingRetry, Some(at)),
        );
        assert_eq!(decide_last(&on(), &rows, &[]), Decision::in_review());
    }

    #[test]
    fn the_budget_counts_phases_not_retried_rows() {
        // One fix phase across two rows (it hit a limit and resumed), and a
        // budget of two: a second fix is still allowed.
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
            ended(row(3, RunKind::Fix, "f1", "b"), ExitClass::UsageLimit),
            row(4, RunKind::Fix, "f1", "c"),
            recorded(row(5, RunKind::Review, "r2", "c")),
        ];
        let findings = [finding(&rows[4], FindingSeverity::High)];
        assert_eq!(
            decide_last(&on(), &rows, &findings),
            Decision::continuing(RunKind::Fix)
        );

        let spent = EffectiveReviewConfig {
            max_review_loops: 1,
            ..on()
        };
        assert_eq!(decide_last(&spent, &rows, &findings), Decision::in_review());
    }

    #[test]
    fn a_rerun_implementation_starts_a_fresh_budget() {
        let config = EffectiveReviewConfig {
            max_review_loops: 1,
            ..on()
        };
        let rows = [
            row(1, RunKind::Implementation, "impl-1", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
            row(3, RunKind::Fix, "f1", "b"),
            recorded(row(4, RunKind::Review, "r2", "b")),
            row(5, RunKind::Implementation, "impl-2", "c"),
            recorded(row(6, RunKind::Review, "r3", "c")),
        ];
        let findings = [finding(&rows[5], FindingSeverity::High)];
        assert_eq!(
            decide_last(&config, &rows, &findings),
            Decision::continuing(RunKind::Fix),
            "last night's fix is history, not this loop's spend",
        );
    }

    #[test]
    fn no_loop_decision_moves_a_task_to_done() {
        let kinds = [RunKind::Implementation, RunKind::Review, RunKind::Fix];
        let classes = [
            ExitClass::Success,
            ExitClass::UsageLimit,
            ExitClass::Transient,
            ExitClass::Interrupted,
            ExitClass::Fatal,
            ExitClass::Cancelled,
        ];
        let mut cases = 0;
        for kind in kinds {
            for exit_class in classes {
                for was_recorded in [true, false] {
                    for head_moved in [true, false] {
                        for window_open in [true, false] {
                            for budget_left in [true, false] {
                                for blocking in [true, false] {
                                    let mut closing = ended(
                                        row(3, kind, "closing", if head_moved { "b" } else { "a" }),
                                        exit_class,
                                    );
                                    closing.findings_recorded = was_recorded;
                                    let rows = [
                                        row(1, RunKind::Implementation, "impl", "a"),
                                        row(2, RunKind::Fix, "f0", "a"),
                                        closing,
                                    ];
                                    let findings = if blocking {
                                        vec![finding(&rows[2], FindingSeverity::Critical)]
                                    } else {
                                        vec![]
                                    };
                                    let config = EffectiveReviewConfig {
                                        max_review_loops: if budget_left { 2 } else { 1 },
                                        ..on()
                                    };
                                    let window = Some(if window_open {
                                        now() + Duration::hours(1)
                                    } else {
                                        now() - Duration::hours(1)
                                    });
                                    for resume_after in [None, Some(now() + Duration::minutes(3))] {
                                        let decision = decide(
                                            &config,
                                            &rows,
                                            &findings,
                                            Closed {
                                                kind,
                                                exit_class,
                                                resume_after,
                                            },
                                            window,
                                            now(),
                                        );
                                        assert_ne!(
                                            decision.landing.column(),
                                            Some(BoardColumn::Done)
                                        );
                                        assert!(matches!(
                                            decision.landing,
                                            Landing::Stays
                                                | Landing::InReview
                                                | Landing::WaitingRetry
                                                | Landing::Failed
                                        ));
                                        // Only a success ever continues, and a
                                        // continuing task stays where it is.
                                        if let NextStep::Continue { .. } = decision.next {
                                            assert_eq!(exit_class, ExitClass::Success);
                                            assert_eq!(decision.landing, Landing::Stays);
                                            assert!(window_open);
                                        }
                                        cases += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(cases, 3 * 6 * 2 * 2 * 2 * 2 * 2 * 2);
    }
}
