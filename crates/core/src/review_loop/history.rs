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

use serde::{Deserialize, Serialize};

use crate::db::{ExitClass, RunKind, RunStatus};
use crate::review::findings::{FindingStatus, ReviewFinding};

use super::{
    current_loop, is_carried_over, loops, open_blocking, EffectiveReviewConfig, Loop, LoopRow,
    Phase,
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
    pub ping_pong: bool,
}

/// One phase, as a history reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhaseSummary {
    pub kind: RunKind,
    /// Oldest first; more than one when the phase was resumed.
    pub run_ids: Vec<String>,
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
    pub resolved: Vec<ReviewFinding>,
}

/// One review, the fix that followed it, and the ping-pong lists.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRound {
    /// `None` only for a fix with no review before it, which no loop writes.
    pub review: Option<PhaseSummary>,
    pub findings: Vec<ReviewFinding>,
    pub fix: Option<FixRound>,
    pub regressed: Vec<ReviewFinding>,
    pub new_after_fix: Vec<ReviewFinding>,
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
    let Some(current) = current_loop(rows) else {
        return Verdict::None;
    };
    let Some(last) = current.phases.last() else {
        return if current.implementation.succeeded() && config.enabled {
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
    let open_blocking = match verdict {
        Verdict::FindingsRemain { open_blocking } => open_blocking,
        _ => 0,
    };
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
            .map(|(index, one)| LoopHistory {
                earlier: index < newest,
                implementation: phase_summary(&one.implementation),
                rounds: rounds(one, findings, config),
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
                findings: raised_by(findings, phase),
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
                        .cloned()
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

fn raised_by(findings: &[ReviewFinding], review: &Phase<'_>) -> Vec<ReviewFinding> {
    findings
        .iter()
        .filter(|finding| review.contains_run(&finding.review_run_id))
        .cloned()
        .collect()
}

fn phase_summary(phase: &Phase<'_>) -> PhaseSummary {
    PhaseSummary {
        kind: phase.kind,
        run_ids: phase.run_ids(),
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
