//! The review-and-fix loop: after a successful implementation, review the work
//! with a fresh agent, fix what it finds, and repeat a bounded number of times
//! (ADR-0017, task 021).
//!
//! A sibling of [`crate::review`], which is what a human or a reviewer
//! *writes*: the verdicts a morning review ends in, and the findings store.
//! This module is the engine that schedules the reviewer and decides what its
//! answer means.
//!
//! # Rows, phases and loops
//!
//! A task's `runs` rows are one sequence across kinds (seam-contract D29
//! point 2). A **phase** is a maximal run of contiguous rows sharing
//! `(kind, session_id)`, D29 point 3's budget boundary: a review that hits a
//! usage limit and resumes is one phase across two rows. Everything here that
//! counts — the budget, the digest's loop number, the witness that a review
//! recorded, and whether `HEAD` moved — reads phases, never rows, so a retried
//! review never spends the budget twice.
//!
//! A **loop** belongs to an implementation phase: the phases after it, up to
//! the next implementation. Run now starts from implementation again, and the
//! budget resets with it.
//!
//! # Who decides
//!
//! [`decide::decide`] is pure, and the board calls it from
//! `outcome::finish_run`'s task-side step on every path that closes a row,
//! reconcile's included (seam-contract D31 point 4). The runner never counts
//! loops and never chooses to continue: a runner that did would be a second
//! copy of ADR-0017's budget, on a machine the board does not control.
//!
//! [`history`]'s builders are pure too, so a view can call them per card over
//! one batched read without copying a rule (task 037).

pub mod board;
pub mod config;
pub mod decide;
pub mod history;

use serde::{Deserialize, Serialize};

use crate::board::{ChangeSummary, ImplementationBase, ReviewContext};
use crate::context::ServiceContext;
use crate::db::{ExitClass, RunKind, RunStatus};
use crate::error::Result;
use crate::review::findings::{self, FindingStatus, ReviewFinding};
use crate::runs::{self, RunReview};

pub use config::{
    effective, effective_instructions, EffectiveReviewConfig, FixSession, ReviewConfig,
    ReviewEnabled, ReviewSettings, TaskReview,
};
pub use decide::{decide, Closed, Decision, Landing};
pub use history::{
    phases, summary, verdict, FixRound, HistoryFinding, LoopHistory, PhaseSummary, ReviewHistory,
    ReviewLoopSummary, ReviewRound, UnreviewedReason, Verdict,
};

/// The projection of a `runs` row the loop reads.
///
/// Loaded by `review::findings::loop_rows`, which stays the one reader of
/// `findings_recorded_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopRow {
    pub id: String,
    pub kind: RunKind,
    pub attempt: i64,
    pub status: RunStatus,
    pub exit_class: Option<ExitClass>,
    pub session_id: String,
    pub head_sha: Option<String>,
    /// Whether this row's review called `record_review_findings`.
    pub findings_recorded: bool,
}

/// Contiguous rows sharing `(kind, session_id)`, with the row before them.
#[derive(Debug, Clone, Copy)]
pub struct Phase<'a> {
    pub kind: RunKind,
    /// Never empty.
    pub rows: &'a [LoopRow],
    /// The row immediately before the phase's first, which is what `HEAD`
    /// moving is measured against.
    pub before: Option<&'a LoopRow>,
}

impl<'a> Phase<'a> {
    pub fn last(&self) -> &'a LoopRow {
        self.rows.last().expect("a phase has at least one row")
    }

    /// Whether the phase ended, and ended well.
    pub fn succeeded(&self) -> bool {
        self.last().status == RunStatus::Succeeded
    }

    /// The phase-level witness (D30 point 7): any of its rows recorded.
    pub fn recorded(&self) -> bool {
        self.rows.iter().any(|row| row.findings_recorded)
    }

    /// Whether the phase left `HEAD` somewhere other than where it found it.
    ///
    /// Across the phase, not per row: a reviewer that committed in its first
    /// row and resumed in its second has still moved `HEAD`. Either value
    /// missing reads as moved, because a branch that cannot be compared is not
    /// a clean one.
    pub fn moved_head(&self) -> bool {
        match (
            self.before.and_then(|row| row.head_sha.as_deref()),
            self.last().head_sha.as_deref(),
        ) {
            (Some(before), Some(after)) => before != after,
            _ => true,
        }
    }

    pub fn contains_run(&self, run_id: &str) -> bool {
        self.rows.iter().any(|row| row.id == run_id)
    }

    pub fn run_ids(&self) -> Vec<String> {
        self.rows.iter().map(|row| row.id.clone()).collect()
    }
}

/// `rows`, oldest first, cut into phases.
pub fn split_phases(rows: &[LoopRow]) -> Vec<Phase<'_>> {
    let mut phases = Vec::new();
    let mut start = 0;
    for end in 1..=rows.len() {
        let boundary = end == rows.len()
            || rows[end].kind != rows[start].kind
            || rows[end].session_id != rows[start].session_id;
        if boundary {
            phases.push(Phase {
                kind: rows[start].kind,
                rows: &rows[start..end],
                before: start.checked_sub(1).map(|index| &rows[index]),
            });
            start = end;
        }
    }
    phases
}

/// One implementation phase and the review and fix phases after it.
#[derive(Debug, Clone)]
pub struct Loop<'a> {
    pub implementation: Phase<'a>,
    pub phases: Vec<Phase<'a>>,
}

impl Loop<'_> {
    /// How many fix phases this loop has spent: the budget's count.
    pub fn fixes_spent(&self) -> u32 {
        self.count(RunKind::Fix)
    }

    /// How many review phases: the loop number.
    pub fn reviews(&self) -> u32 {
        self.count(RunKind::Review)
    }

    fn count(&self, kind: RunKind) -> u32 {
        let count = self
            .phases
            .iter()
            .filter(|phase| phase.kind == kind)
            .count();
        u32::try_from(count).unwrap_or(u32::MAX)
    }
}

/// Every loop in `rows`, oldest first. Rows before the first implementation
/// belong to no loop.
pub fn loops(rows: &[LoopRow]) -> Vec<Loop<'_>> {
    let mut loops: Vec<Loop<'_>> = Vec::new();
    for phase in split_phases(rows) {
        if phase.kind == RunKind::Implementation {
            loops.push(Loop {
                implementation: phase,
                phases: Vec::new(),
            });
        } else if let Some(current) = loops.last_mut() {
            current.phases.push(phase);
        }
    }
    loops
}

/// The loop of the newest implementation phase, if the task has one.
pub fn current_loop(rows: &[LoopRow]) -> Option<Loop<'_>> {
    loops(rows).pop()
}

/// The findings `review` raised that are still open and meet `threshold`.
pub fn open_blocking<'f>(
    findings: &'f [ReviewFinding],
    review: &Phase<'_>,
    threshold: crate::review::FindingSeverity,
) -> Vec<&'f ReviewFinding> {
    findings
        .iter()
        .filter(|finding| {
            review.contains_run(&finding.review_run_id)
                && finding.status == FindingStatus::Open
                && finding.severity.is_at_least(threshold)
        })
        .collect()
}

/// The findings `review` raised that are still open and sit below
/// `threshold`: raised, and advisory because they did not start a fix.
pub fn open_advisory<'f>(
    findings: &'f [ReviewFinding],
    review: &Phase<'_>,
    threshold: crate::review::FindingSeverity,
) -> Vec<&'f ReviewFinding> {
    findings
        .iter()
        .filter(|finding| {
            review.contains_run(&finding.review_run_id)
                && finding.status == FindingStatus::Open
                && !finding.severity.is_at_least(threshold)
        })
        .collect()
}

/// What a review or fix phase is composed from, read for `task_id` as it
/// stands (seam-contract D31 point 6).
pub async fn context(
    ctx: &ServiceContext,
    task_id: &str,
    resolved: config::Resolved,
) -> Result<ReviewContext> {
    let rows = findings::loop_rows(ctx, task_id).await?;
    let all = findings::list(ctx, task_id, None).await?;
    let current = current_loop(&rows);

    let newest_review = current.as_ref().and_then(|current| {
        current
            .phases
            .iter()
            .rev()
            .find(|phase| phase.kind == RunKind::Review)
            .copied()
    });
    let open_blocking = newest_review
        .map(|review| {
            open_blocking(&all, &review, resolved.config.blocking_severity)
                .into_iter()
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let rejected = all
        .iter()
        .filter(|finding| finding.status == FindingStatus::Rejected && !is_carried_over(finding))
        .cloned()
        .collect();

    let implementation = rows
        .iter()
        .rev()
        .find(|row| row.kind == RunKind::Implementation)
        .map(|row| row.id.clone());
    let implementation = match implementation {
        Some(id) => {
            let run = runs::get_run_row(ctx, &id).await?;
            Some(ImplementationBase {
                session_id: run.session_id,
                base_ref: run.base_ref,
                base_sha: run.base_sha,
            })
        }
        None => None,
    };

    let newest = rows.last();
    let change = match newest {
        Some(row) => match runs::get_run(ctx, &row.id).await?.review {
            RunReview::Recorded {
                bundle: Some(bundle),
            } => Some(ChangeSummary {
                diff: bundle.diff,
                files: bundle.files,
            }),
            _ => None,
        },
        None => None,
    };
    let phase_recorded = split_phases(&rows)
        .last()
        .is_some_and(|phase| phase.kind == RunKind::Review && phase.recorded());

    Ok(ReviewContext {
        instructions: resolved.instructions,
        config: resolved.config,
        open_blocking,
        rejected,
        implementation,
        head_sha: newest.and_then(|row| row.head_sha.clone()),
        change,
        phase_recorded,
    })
}

/// A finding stored `rejected` because a fixer had already rejected its
/// fingerprint, rather than one a fixer rejected itself: no run resolved it.
pub fn is_carried_over(finding: &ReviewFinding) -> bool {
    finding.status == FindingStatus::Rejected && finding.resolved_by_run_id.is_none()
}

/// The single-task loader for [`history::summary`]: what `get_task` carries.
pub async fn summary_for(
    ctx: &ServiceContext,
    task_id: &str,
    config: &EffectiveReviewConfig,
) -> Result<Option<ReviewLoopSummary>> {
    let rows = findings::loop_rows(ctx, task_id).await?;
    let all = findings::list(ctx, task_id, None).await?;
    Ok(summary(&rows, &all, config))
}

/// The single-task loader for [`history::phases`]. Task 037 adds the command
/// and the tool that reach it.
pub async fn history(ctx: &ServiceContext, task_id: &str) -> Result<ReviewHistory> {
    let task = crate::tasks::service::fetch_task_row(&ctx.pool, task_id).await?;
    let resolved = config::resolve(&ctx.pool, task_id, &task.repository_id).await?;
    let rows = findings::loop_rows(ctx, task_id).await?;
    let all = findings::list(ctx, task_id, None).await?;
    Ok(phases(&rows, &all, &resolved.config))
}

#[cfg(test)]
pub(crate) mod test_rows {
    //! Row builders the unit tests in this module's children share.

    use super::LoopRow;
    use crate::db::{ExitClass, RunKind, RunStatus};

    pub fn row(attempt: i64, kind: RunKind, session: &str, head: &str) -> LoopRow {
        LoopRow {
            id: format!("run-{attempt}"),
            kind,
            attempt,
            status: RunStatus::Succeeded,
            exit_class: Some(ExitClass::Success),
            session_id: session.to_string(),
            head_sha: Some(head.to_string()),
            findings_recorded: false,
        }
    }

    pub fn recorded(mut row: LoopRow) -> LoopRow {
        row.findings_recorded = true;
        row
    }

    pub fn ended(mut row: LoopRow, exit_class: ExitClass) -> LoopRow {
        row.exit_class = Some(exit_class);
        row.status = match exit_class {
            ExitClass::Success => RunStatus::Succeeded,
            ExitClass::Cancelled => RunStatus::Cancelled,
            ExitClass::Interrupted => RunStatus::Interrupted,
            _ => RunStatus::Failed,
        };
        row
    }
}

#[cfg(test)]
mod tests {
    use super::test_rows::*;
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_phase_is_contiguous_rows_sharing_kind_and_session() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            row(2, RunKind::Review, "r1", "a"),
            row(3, RunKind::Review, "r1", "a"),
            row(4, RunKind::Fix, "impl", "b"),
            row(5, RunKind::Review, "r2", "b"),
        ];

        let phases = split_phases(&rows);

        let shape: Vec<(RunKind, Vec<i64>)> = phases
            .iter()
            .map(|phase| {
                (
                    phase.kind,
                    phase.rows.iter().map(|row| row.attempt).collect(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                (RunKind::Implementation, vec![1]),
                (RunKind::Review, vec![2, 3]),
                (RunKind::Fix, vec![4]),
                (RunKind::Review, vec![5]),
            ]
        );
        assert_eq!(phases[1].before.map(|row| row.attempt), Some(1));
    }

    #[test]
    fn head_moved_compares_the_phases_last_row_with_the_row_before_it() {
        let rows = [
            row(1, RunKind::Implementation, "impl", "a"),
            row(2, RunKind::Review, "r1", "b"),
            row(3, RunKind::Review, "r1", "b"),
        ];
        assert!(split_phases(&rows)[1].moved_head());

        let unmoved = [
            row(1, RunKind::Implementation, "impl", "a"),
            row(2, RunKind::Review, "r1", "a"),
        ];
        assert!(!split_phases(&unmoved)[1].moved_head());

        let mut unknown = unmoved.clone();
        unknown[0].head_sha = None;
        assert!(
            split_phases(&unknown)[1].moved_head(),
            "missing reads as moved"
        );
    }

    #[test]
    fn a_rerun_implementation_starts_a_new_loop() {
        let rows = [
            row(1, RunKind::Implementation, "impl-1", "a"),
            row(2, RunKind::Review, "r1", "a"),
            row(3, RunKind::Fix, "f1", "b"),
            row(4, RunKind::Implementation, "impl-2", "c"),
            row(5, RunKind::Review, "r2", "c"),
        ];

        let all = loops(&rows);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].fixes_spent(), 1);
        let current = current_loop(&rows).expect("a loop");
        assert_eq!(current.implementation.rows[0].attempt, 4);
        assert_eq!(current.fixes_spent(), 0);
        assert_eq!(current.reviews(), 1);
    }
}
