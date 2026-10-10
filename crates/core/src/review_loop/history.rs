//! What a task's review loop came to, as pure builders over its rows and
//! findings (ADR-0017, task 021).
//!
//! [`verdict`], [`summary`] and [`phases`] take what one batched read returns
//! and compute nothing else, so task 037 can call them per card without a
//! query of its own and without copying a rule. The single-task loaders are
//! `review_loop::{summary_for, history}` and `get_task`.
//!
//! # "Flagged" is derived and never stored
//!
//! A task is flagged when its verdict is [`Verdict::FindingsRemain`] or
//! [`Verdict::Unreviewed`]. There is no column for it, for D29 point 8's
//! reason: a stored flag is a second source of truth for what the rows
//! already say.
//!
//! # Ping-pong is a signal, not a verdict
//!
//! For each review after the first in a loop, the history names findings the
//! preceding fix marked fixed and the reviewer raised again (`regressed`), and
//! blocking findings the preceding review did not raise (`new_after_fix`). A
//! fresh reviewer may simply notice something the first one missed, so the
//! card says "may be going in circles", not "the fix broke it". The budget
//! still bounds the loop; the signal is there so a ping-pong is not silently
//! absorbed by it.

use std::collections::HashSet;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

use crate::db::{ExitClass, RunKind, RunStatus};
use crate::review::findings::{FindingStatus, ReviewFinding};

use super::{
    current_loop, is_carried_over, loops, open_advisory, open_blocking, EffectiveReviewConfig,
    Loop, LoopRow, Phase,
};

/// Why a loop did not end clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnreviewedReason {
    /// The implementation succeeded and no review has run, or one is running.
    NotReviewed,
    /// The review failed, was cancelled, or ran out of retries. A review
    /// rewritten to `fatal` for leaving tracked changes is one of these, and its
    /// row's message says why.
    ReviewFailed,
    /// The review finished without calling `record_review_findings`.
    NothingRecorded,
    /// The review moved `HEAD`: it changed the branch it was asked to judge.
    ReviewChangedBranch,
    /// The loop ended on a fix, which nothing reviewed.
    FixNotReviewed,
}

/// What the loop says about the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "verdict",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Verdict {
    /// The loop is off and the task has no loop rows.
    None,
    Clean,
    FindingsRemain {
        open_blocking: u32,
    },
    Unreviewed {
        reason: UnreviewedReason,
    },
}

impl Verdict {
    /// [`FindingsRemain`](Verdict::FindingsRemain) or
    /// [`Unreviewed`](Verdict::Unreviewed): what the card flags.
    pub const fn is_flagged(self) -> bool {
        matches!(
            self,
            Verdict::FindingsRemain { .. } | Verdict::Unreviewed { .. }
        )
    }
}

/// What a card and `get_task` carry about the loop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewLoopSummary {
    /// The effective setting now, which may differ from when the loop ran.
    pub enabled: bool,
    pub max_review_loops: u32,
    pub fixes_spent: u32,
    /// Review phases in the current loop: the loop number. The digest reports
    /// the same count.
    pub reviews: u32,
    pub verdict: Verdict,
    pub open_blocking: u32,
    /// Open findings of the newest review that sit below `blocking_severity`:
    /// raised, and not worth a fix. Zero unless the verdict is `Clean` or
    /// `FindingsRemain`, for the reason `open_blocking` is.
    pub open_advisory: u32,
    pub ping_pong: bool,
}

/// A stored finding and whether it blocks, as the history carries it.
///
/// `blocking` is the configuration's rule applied here, in core, so a view
/// that marks a finding advisory never compares severities itself (task 037).
/// It describes the finding against the *current* effective
/// `blocking_severity`, like every other figure the history derives.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryFinding {
    #[serde(flatten)]
    pub finding: ReviewFinding,
    pub blocking: bool,
    /// Stored `rejected` because a fix had already rejected the same finding,
    /// not because a fix rejected this one: its `resolution` already says
    /// "Rejected earlier as ..." and no run resolved it.
    pub carried_over: bool,
}

impl HistoryFinding {
    fn new(finding: &ReviewFinding, config: &EffectiveReviewConfig) -> Self {
        Self {
            blocking: finding.severity.is_at_least(config.blocking_severity),
            carried_over: is_carried_over(finding),
            finding: finding.clone(),
        }
    }
}

impl Deref for HistoryFinding {
    type Target = ReviewFinding;

    fn deref(&self) -> &ReviewFinding {
        &self.finding
    }
}

/// One phase, as a history reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseSummary {
    pub kind: RunKind,
    /// Oldest first; more than one when the phase was resumed.
    pub run_ids: Vec<String>,
    /// The `attempt` of each run in [`run_ids`](PhaseSummary::run_ids), so a
    /// view can say `Review #4` and `Fixed in #6` without a second read.
    pub attempts: Vec<i64>,
    /// The last row's.
    pub status: RunStatus,
    pub exit_class: Option<ExitClass>,
}

/// A fix, and what it resolved.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixRound {
    pub phase: PhaseSummary,
    /// The findings its rows resolved, fixed or rejected.
    pub resolved: Vec<HistoryFinding>,
}

/// One review, the fix that followed it, and the ping-pong lists.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRound {
    /// `None` only for a fix with no review before it, which no loop writes.
    pub review: Option<PhaseSummary>,
    pub findings: Vec<HistoryFinding>,
    pub fix: Option<FixRound>,
    pub regressed: Vec<HistoryFinding>,
    pub new_after_fix: Vec<HistoryFinding>,
    pub ping_pong: bool,
}

/// One loop: an implementation and the rounds after it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopHistory {
    /// Before a re-run implementation: not the loop the verdict is about.
    pub earlier: bool,
    pub implementation: PhaseSummary,
    pub rounds: Vec<ReviewRound>,
    /// Fix phases this loop spent.
    pub fixes_spent: u32,
    /// What this loop says about its branch, by the rule [`verdict`] applies
    /// to the newest one. An earlier loop that never got a review reads
    /// [`Verdict::None`]: whether it would have been reviewed is not a
    /// question the configuration of today answers about the past.
    pub verdict: Verdict,
    /// As [`ReviewLoopSummary`]'s, for this loop.
    pub open_blocking: u32,
    pub open_advisory: u32,
}

/// Every loop the task has had, oldest first.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewHistory {
    pub loops: Vec<LoopHistory>,
}

/// What the current loop says about the branch.
///
/// A succeeded implementation with nothing after it is the one case that
/// reads the configuration rather than the rows: `not_reviewed` while the loop
/// is effectively on, `None` while it is off. That is true whenever it is
/// shown, which is the property that matters.
pub fn verdict(
    rows: &[LoopRow],
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> Verdict {
    match current_loop(rows) {
        Some(current) => loop_verdict(&current, true, findings, config),
        None => Verdict::None,
    }
}

/// [`verdict`] for one loop. `newest` is whether the configuration may speak
/// for it: only the newest loop can still be reviewed.
fn loop_verdict(
    one: &Loop<'_>,
    newest: bool,
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> Verdict {
    let Some(last) = one.phases.last() else {
        return if newest && one.implementation.succeeded() && config.enabled {
            Verdict::Unreviewed {
                reason: UnreviewedReason::NotReviewed,
            }
        } else {
            Verdict::None
        };
    };

    let reason = match last.kind {
        RunKind::Fix => UnreviewedReason::FixNotReviewed,
        RunKind::Implementation => unreachable!("a loop's phases follow its implementation"),
        RunKind::Review if last.last().status == RunStatus::Running => {
            UnreviewedReason::NotReviewed
        }
        RunKind::Review if !last.succeeded() => UnreviewedReason::ReviewFailed,
        RunKind::Review if last.moved_head() => UnreviewedReason::ReviewChangedBranch,
        RunKind::Review if !last.recorded() => UnreviewedReason::NothingRecorded,
        RunKind::Review => {
            let open = count(open_blocking(findings, last, config.blocking_severity).len());
            return if open == 0 {
                Verdict::Clean
            } else {
                Verdict::FindingsRemain {
                    open_blocking: open,
                }
            };
        }
    };
    Verdict::Unreviewed { reason }
}

/// The open blocking and open advisory findings a loop's verdict stands on:
/// its newest review's, when the verdict is about that review at all.
fn open_counts(
    one: &Loop<'_>,
    verdict: Verdict,
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> (u32, u32) {
    match (verdict, one.phases.last()) {
        (Verdict::Clean | Verdict::FindingsRemain { .. }, Some(review)) => (
            count(open_blocking(findings, review, config.blocking_severity).len()),
            count(open_advisory(findings, review, config.blocking_severity).len()),
        ),
        _ => (0, 0),
    }
}

/// The card's summary, or `None` when the verdict is
/// [`None`](Verdict::None): a task the loop never touched carries nothing.
pub fn summary(
    rows: &[LoopRow],
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> Option<ReviewLoopSummary> {
    let verdict = verdict(rows, findings, config);
    if verdict == Verdict::None {
        return None;
    }
    let current = current_loop(rows)?;
    let (open_blocking, open_advisory) = open_counts(&current, verdict, findings, config);
    let ping_pong = rounds(&current, findings, config)
        .iter()
        .any(|round| round.ping_pong);

    Some(ReviewLoopSummary {
        enabled: config.enabled,
        max_review_loops: config.max_review_loops,
        fixes_spent: current.fixes_spent(),
        reviews: current.reviews(),
        verdict,
        open_blocking,
        open_advisory,
        ping_pong,
    })
}

/// Every loop's phases, each review with its findings, the fix that followed
/// it and what that fix resolved, and the ping-pong lists. Loops before the
/// newest implementation are included and marked as earlier.
///
/// Takes the configuration as well as the rows and findings, because
/// `new_after_fix` counts only blocking findings, and what blocks is the
/// configuration's `blocking_severity`.
pub fn phases(
    rows: &[LoopRow],
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> ReviewHistory {
    let all = loops(rows);
    let newest = all.len().saturating_sub(1);
    ReviewHistory {
        loops: all
            .iter()
            .enumerate()
            .map(|(index, one)| {
                let verdict = loop_verdict(one, index == newest, findings, config);
                let (open_blocking, open_advisory) = open_counts(one, verdict, findings, config);
                LoopHistory {
                    earlier: index < newest,
                    implementation: phase_summary(&one.implementation),
                    rounds: rounds(one, findings, config),
                    fixes_spent: one.fixes_spent(),
                    verdict,
                    open_blocking,
                    open_advisory,
                }
            })
            .collect(),
    }
}

fn rounds(
    one: &Loop<'_>,
    findings: &[ReviewFinding],
    config: &EffectiveReviewConfig,
) -> Vec<ReviewRound> {
    let mut rounds: Vec<ReviewRound> = Vec::new();
    for phase in &one.phases {
        match phase.kind {
            RunKind::Review => rounds.push(ReviewRound {
                review: Some(phase_summary(phase)),
                findings: raised_by(findings, phase, config),
                fix: None,
                regressed: Vec::new(),
                new_after_fix: Vec::new(),
                ping_pong: false,
            }),
            RunKind::Fix => {
                let fix = FixRound {
                    phase: phase_summary(phase),
                    resolved: findings
                        .iter()
                        .filter(|finding| {
                            finding
                                .resolved_by_run_id
                                .as_deref()
                                .is_some_and(|run| phase.contains_run(run))
                        })
                        .map(|finding| HistoryFinding::new(finding, config))
                        .collect(),
                };
                match rounds.last_mut() {
                    Some(round) if round.fix.is_none() => round.fix = Some(fix),
                    _ => rounds.push(ReviewRound {
                        review: None,
                        findings: Vec::new(),
                        fix: Some(fix),
                        regressed: Vec::new(),
                        new_after_fix: Vec::new(),
                        ping_pong: false,
                    }),
                }
            }
            RunKind::Implementation => {}
        }
    }

    for index in 1..rounds.len() {
        let (before, after) = rounds.split_at_mut(index);
        let previous = &before[index - 1];
        let round = &mut after[0];
        if round.review.is_none() {
            continue;
        }

        let fixed: HashSet<&str> = previous
            .fix
            .iter()
            .flat_map(|fix| &fix.resolved)
            .filter(|finding| finding.status == FindingStatus::Fixed)
            .filter_map(|finding| finding.fingerprint.as_deref())
            .collect();
        let raised_before: HashSet<&str> = previous
            .findings
            .iter()
            .filter_map(|finding| finding.fingerprint.as_deref())
            .collect();

        round.regressed = round
            .findings
            .iter()
            .filter(|finding| {
                finding
                    .fingerprint
                    .as_deref()
                    .is_some_and(|print| fixed.contains(print))
            })
            .cloned()
            .collect();
        round.new_after_fix = round
            .findings
            .iter()
            .filter(|finding| {
                finding.severity.is_at_least(config.blocking_severity)
                    && !is_carried_over(finding)
                    && finding
                        .fingerprint
                        .as_deref()
                        .is_some_and(|print| !raised_before.contains(print))
            })
            .cloned()
            .collect();
        round.ping_pong = !round.regressed.is_empty() || !round.new_after_fix.is_empty();
    }
    rounds
}

fn raised_by(
    findings: &[ReviewFinding],
    review: &Phase<'_>,
    config: &EffectiveReviewConfig,
) -> Vec<HistoryFinding> {
    findings
        .iter()
        .filter(|finding| review.contains_run(&finding.review_run_id))
        .map(|finding| HistoryFinding::new(finding, config))
        .collect()
}

fn phase_summary(phase: &Phase<'_>) -> PhaseSummary {
    PhaseSummary {
        kind: phase.kind,
        run_ids: phase.run_ids(),
        attempts: phase.rows.iter().map(|row| row.attempt).collect(),
        status: phase.last().status,
        exit_class: phase.last().exit_class,
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review::findings::{fingerprint, FindingSeverity};
    use crate::review_loop::test_rows::{ended, recorded, row};
    use pretty_assertions::assert_eq;

    fn on() -> EffectiveReviewConfig {
        EffectiveReviewConfig {
            enabled: true,
            ..EffectiveReviewConfig::default()
        }
    }

    fn found(
        id: &str,
        review: &str,
        severity: FindingSeverity,
        title: &str,
        status: FindingStatus,
        resolved_by: Option<&str>,
    ) -> ReviewFinding {
        ReviewFinding {
            id: id.to_string(),
            task_id: "task".to_string(),
            review_run_id: review.to_string(),
            ordinal: 0,
            severity,
            title: title.to_string(),
            body: "Explained.".to_string(),
            file: Some("src/lib.rs".to_string()),
            line: Some(1),
            fingerprint: Some(fingerprint(Some("src/lib.rs"), title)),
            status,
            resolution: resolved_by.map(|_| "done".to_string()),
            resolved_by_run_id: resolved_by.map(str::to_string),
            created_at: "2026-08-20T03:00:00Z".parse().expect("a timestamp"),
            resolved_at: None,
        }
    }

    /// Implementation, review, fix, review.
    fn two_reviews() -> Vec<LoopRow> {
        vec![
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
            row(3, RunKind::Fix, "f1", "b"),
            recorded(row(4, RunKind::Review, "r2", "b")),
        ]
    }

    #[test]
    fn a_new_blocking_finding_after_a_fix_is_reported_as_ping_pong() {
        let rows = two_reviews();
        let findings = [
            found(
                "a",
                "run-2",
                FindingSeverity::High,
                "First",
                FindingStatus::Fixed,
                Some("run-3"),
            ),
            found(
                "b",
                "run-4",
                FindingSeverity::High,
                "Second",
                FindingStatus::Open,
                None,
            ),
            found(
                "c",
                "run-4",
                FindingSeverity::Low,
                "Minor",
                FindingStatus::Open,
                None,
            ),
        ];

        let history = phases(&rows, &findings, &on());

        let second = &history.loops[0].rounds[1];
        assert_eq!(
            second
                .new_after_fix
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"],
            "a low finding below the threshold is not blocking",
        );
        assert!(second.regressed.is_empty());
        assert!(second.ping_pong);
        assert!(summary(&rows, &findings, &on()).expect("a loop").ping_pong);
    }

    #[test]
    fn a_fixed_finding_raised_again_is_reported_as_regressed() {
        let rows = two_reviews();
        let findings = [
            found(
                "a",
                "run-2",
                FindingSeverity::High,
                "Same",
                FindingStatus::Fixed,
                Some("run-3"),
            ),
            found(
                "b",
                "run-4",
                FindingSeverity::Low,
                "  SAME  ",
                FindingStatus::Open,
                None,
            ),
        ];

        let history = phases(&rows, &findings, &on());

        let second = &history.loops[0].rounds[1];
        assert_eq!(
            second
                .regressed
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            vec!["b"]
        );
        assert!(second.new_after_fix.is_empty(), "it was raised before");
        assert!(second.ping_pong);
        assert_eq!(
            history.loops[0].rounds[0]
                .fix
                .as_ref()
                .map(|fix| fix.resolved.len()),
            Some(1)
        );
    }

    #[test]
    fn a_single_review_never_reports_ping_pong() {
        let rows = &two_reviews()[..2];
        let findings = [found(
            "a",
            "run-2",
            FindingSeverity::Critical,
            "Only",
            FindingStatus::Open,
            None,
        )];

        let history = phases(rows, &findings, &on());

        assert_eq!(history.loops[0].rounds.len(), 1);
        assert!(!history.loops[0].rounds[0].ping_pong);
        assert!(!summary(rows, &findings, &on()).expect("a loop").ping_pong);
    }

    #[test]
    fn earlier_loops_are_kept_and_marked() {
        let mut rows = two_reviews();
        rows.push(row(5, RunKind::Implementation, "impl-2", "c"));

        let history = phases(&rows, &[], &on());

        assert_eq!(
            history
                .loops
                .iter()
                .map(|one| one.earlier)
                .collect::<Vec<_>>(),
            vec![true, false]
        );
        assert_eq!(history.loops[0].rounds.len(), 2);
        assert!(history.loops[1].rounds.is_empty());
    }

    #[test]
    fn a_finding_below_the_blocking_severity_is_advisory_and_counted_apart() {
        let rows = two_reviews();
        let findings = [
            found(
                "b",
                "run-4",
                FindingSeverity::High,
                "Blocks",
                FindingStatus::Open,
                None,
            ),
            found(
                "n",
                "run-4",
                FindingSeverity::Low,
                "Nit",
                FindingStatus::Open,
                None,
            ),
        ];

        let summary = summary(&rows, &findings, &on()).expect("a loop");
        assert_eq!((summary.open_blocking, summary.open_advisory), (1, 1));

        let history = phases(&rows, &findings, &on());
        let flags: Vec<(&str, bool)> = history.loops[0].rounds[1]
            .findings
            .iter()
            .map(|finding| (finding.id.as_str(), finding.blocking))
            .collect();
        assert_eq!(flags, vec![("b", true), ("n", false)]);
        let newest = &history.loops[0];
        assert_eq!((newest.open_blocking, newest.open_advisory), (1, 1));

        let only_a_nit = &findings[1..];
        let summary = super::summary(&rows, only_a_nit, &on()).expect("a loop");
        assert_eq!(summary.verdict, Verdict::Clean);
        assert_eq!((summary.open_blocking, summary.open_advisory), (0, 1));
    }

    #[test]
    fn what_blocks_follows_the_effective_blocking_severity() {
        let rows = two_reviews();
        let findings = [found(
            "n",
            "run-4",
            FindingSeverity::Low,
            "Nit",
            FindingStatus::Open,
            None,
        )];
        let strict = EffectiveReviewConfig {
            blocking_severity: FindingSeverity::Low,
            ..on()
        };

        let history = phases(&rows, &findings, &strict);

        assert!(history.loops[0].rounds[1].findings[0].blocking);
        assert_eq!(history.loops[0].open_blocking, 1);
        assert_eq!(history.loops[0].open_advisory, 0);
    }

    #[test]
    fn each_loop_carries_its_own_verdict_and_fixes() {
        let mut rows = two_reviews();
        rows.push(row(5, RunKind::Implementation, "impl-2", "c"));

        let history = phases(&rows, &[], &on());

        assert_eq!(history.loops[0].verdict, Verdict::Clean);
        assert_eq!(history.loops[0].fixes_spent, 1);
        assert_eq!(
            history.loops[1].verdict,
            Verdict::Unreviewed {
                reason: UnreviewedReason::NotReviewed
            },
            "the newest loop reads the configuration"
        );
        assert_eq!(history.loops[1].fixes_spent, 0);

        let off = phases(&rows, &[], &EffectiveReviewConfig::default());
        assert_eq!(off.loops[1].verdict, Verdict::None);
    }

    #[test]
    fn a_phase_names_the_attempt_of_every_row_it_spans() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            recorded(row(2, RunKind::Review, "r1", "a")),
            recorded(row(3, RunKind::Review, "r1", "a")),
        ];

        let history = phases(&rows, &[], &on());

        let review = history.loops[0].rounds[0]
            .review
            .as_ref()
            .expect("a review");
        assert_eq!(review.attempts, vec![2, 3]);
        assert_eq!(history.loops[0].implementation.attempts, vec![1]);
    }

    #[test]
    fn a_finding_a_fix_had_already_rejected_is_marked_carried_over() {
        let rows = two_reviews();
        let mut carried = found(
            "c",
            "run-4",
            FindingSeverity::High,
            "Declined",
            FindingStatus::Rejected,
            None,
        );
        carried.resolution = Some("Rejected earlier as a: not a bug".to_string());
        let own = found(
            "o",
            "run-2",
            FindingSeverity::High,
            "Declined here",
            FindingStatus::Rejected,
            Some("run-3"),
        );

        let history = phases(&rows, &[own, carried], &on());

        assert!(!history.loops[0].rounds[0].findings[0].carried_over);
        assert!(history.loops[0].rounds[1].findings[0].carried_over);
    }

    #[test]
    fn every_verdict_reads_off_the_last_phase() {
        let config = on();
        let implementation = [row(1, RunKind::Implementation, "impl", "a")];
        assert_eq!(
            verdict(&implementation, &[], &EffectiveReviewConfig::default()),
            Verdict::None
        );
        assert_eq!(
            verdict(&implementation, &[], &config),
            Verdict::Unreviewed {
                reason: UnreviewedReason::NotReviewed
            }
        );

        let rows = two_reviews();
        assert_eq!(verdict(&rows, &[], &config), Verdict::Clean);
        assert_eq!(
            verdict(&rows[..3], &[], &config),
            Verdict::Unreviewed {
                reason: UnreviewedReason::FixNotReviewed
            }
        );

        let blocking = [found(
            "b",
            "run-4",
            FindingSeverity::High,
            "Left",
            FindingStatus::Open,
            None,
        )];
        assert_eq!(
            verdict(&rows, &blocking, &config),
            Verdict::FindingsRemain { open_blocking: 1 }
        );

        let mut silent = rows.clone();
        silent[3].findings_recorded = false;
        assert_eq!(
            verdict(&silent, &[], &config),
            Verdict::Unreviewed {
                reason: UnreviewedReason::NothingRecorded
            }
        );

        let mut moved = rows.clone();
        moved[3].head_sha = Some("z".to_string());
        assert_eq!(
            verdict(&moved, &[], &config),
            Verdict::Unreviewed {
                reason: UnreviewedReason::ReviewChangedBranch
            }
        );

        let mut failed = rows.clone();
        failed[3] = ended(failed[3].clone(), ExitClass::Fatal);
        assert_eq!(
            verdict(&failed, &[], &config),
            Verdict::Unreviewed {
                reason: UnreviewedReason::ReviewFailed
            }
        );
    }
}
