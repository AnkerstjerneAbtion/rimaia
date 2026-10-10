//! What a run would execute, piece by piece, and whether a runner's owner
//! consents to each (ADR-0032 points 3 and 6).
//!
//! Pure: values in, values out, no pool. [`super`] reads the values, inside
//! the transaction that acts on the answer.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::board::LeasePurpose;
use crate::events::{RunId, RunnerId, TaskId, UserId};

/// One kind of consent-gated content, in the spelling of the
/// `acceptances.content` `CHECK` (seam-contract D28 part 6).
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    sqlx::Type,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum ContentKind {
    /// A task's `plan` and `extra_instructions`, one revision for both.
    Plan,
    /// A task's own override of the review instructions.
    TaskReviewInstructions,
    /// The team's base instructions.
    BaseInstructions,
    /// The team's review instructions.
    ReviewInstructions,
    /// Findings a run on another runner recorded, which the composer includes.
    ReviewFindings,
    /// The dependency commit a task's worktree starts from (ADR-0033).
    BaseCommit,
}

impl ContentKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            ContentKind::Plan => "plan",
            ContentKind::TaskReviewInstructions => "task_review_instructions",
            ContentKind::BaseInstructions => "base_instructions",
            ContentKind::ReviewInstructions => "review_instructions",
            ContentKind::ReviewFindings => "review_findings",
            ContentKind::BaseCommit => "base_commit",
        }
    }

    /// The two kinds a team holds rather than a task: an acceptance of them
    /// names no task, mirroring the table's `CHECK`.
    pub const fn is_team_wide(self) -> bool {
        matches!(
            self,
            ContentKind::BaseInstructions | ContentKind::ReviewInstructions
        )
    }
}

impl fmt::Display for ContentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One piece of content a run would execute, at the revision it would
/// execute.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    pub kind: ContentKind,
    /// The task the content belongs to: the task itself, or for
    /// [`ContentKind::BaseCommit`] the dependency that produced the commit.
    /// `None` for the two team-wide kinds.
    pub task_id: Option<TaskId>,
    /// The integer revision in decimal for plan and instructions, the run's
    /// id for findings, the commit for a base commit.
    pub revision: String,
    /// `None` for a deleted account: "a former member".
    pub author: Option<UserId>,
    /// ADR-0032 point 6's mark: written with the author's credentials while
    /// one of their runners ran someone else's content.
    pub written_during_run: bool,
}

/// A revisioned text as consent reads it: its current revision, who wrote
/// it, and point 6's mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Revision {
    pub revision: i64,
    pub author: Option<UserId>,
    pub written_during_run: bool,
}

/// A run whose recorded text a composer includes, and who owns the runner it
/// ran on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOnRunner {
    pub run_id: RunId,
    /// `None` once the runner's row is gone.
    pub runner_id: Option<RunnerId>,
    /// The runner's owner, `None` when either is gone.
    pub owner: Option<UserId>,
}

/// The dependency commit a worktree starts from, and the owner of every
/// runner whose work it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseCommitInput {
    pub task_id: TaskId,
    pub commit: String,
    /// Distinct, in a stable order. `None` stands for a runner or an owner
    /// that is gone.
    pub owners: Vec<Option<UserId>>,
}

/// Everything [`pieces_for`] chooses from, for one task and one claiming
/// runner. Absent or empty content is `None`, or an empty list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PieceInputs {
    pub task_id: TaskId,
    /// The runner that would run the content: findings it recorded itself
    /// need no consent.
    pub claiming_runner: RunnerId,
    /// `None` when both the plan and the extra instructions are empty.
    pub plan: Option<Revision>,
    pub base_instructions: Option<Revision>,
    /// The task's override, `None` when it is blank.
    pub task_review_instructions: Option<Revision>,
    pub team_review_instructions: Option<Revision>,
    /// For `fix`: the review runs whose open findings the fix acts on.
    pub findings_to_fix: Vec<RunOnRunner>,
    /// For `review`: the fix runs whose rejection reasons fill
    /// `# Findings already rejected`.
    pub rejections: Vec<RunOnRunner>,
    /// Present only when the base is a dependency's commit.
    pub base_commit: Option<BaseCommitInput>,
}

/// The consent-gated content the composer for `purpose` reads, in the order
/// the prompt reads it.
///
/// | Purpose | Pieces |
/// | --- | --- |
/// | `implementation` | plan · base instructions · base commit |
/// | `strategy` | plan · base commit |
/// | `review` | plan · the effective review instructions · findings from another runner · base commit |
/// | `fix` | plan · base instructions · findings from another runner · base commit |
///
/// The planner takes no base instructions (ADR-0009's 2026-08-28 amendment)
/// and borrows the implementation's worktree, so its base commit is a piece.
/// The strategy guidance an implementation prompt carries is not a piece:
/// ADR-0032 point 3 exempts execution strategy.
pub fn pieces_for(purpose: LeasePurpose, inputs: &PieceInputs) -> Vec<Piece> {
    let task = &inputs.task_id;
    let mut pieces = Vec::new();

    if let Some(plan) = &inputs.plan {
        pieces.push(revisioned(ContentKind::Plan, Some(task), plan));
    }

    match purpose {
        LeasePurpose::Implementation | LeasePurpose::Fix => {
            if let Some(base) = &inputs.base_instructions {
                pieces.push(revisioned(ContentKind::BaseInstructions, None, base));
            }
        }
        LeasePurpose::Review => {
            // 021's rule: the task's override when it says something, the
            // team's otherwise.
            match (
                &inputs.task_review_instructions,
                &inputs.team_review_instructions,
            ) {
                (Some(own), _) => {
                    pieces.push(revisioned(
                        ContentKind::TaskReviewInstructions,
                        Some(task),
                        own,
                    ));
                }
                (None, Some(team)) => {
                    pieces.push(revisioned(ContentKind::ReviewInstructions, None, team));
                }
                (None, None) => {}
            }
        }
        LeasePurpose::Strategy => {}
    }

    let findings = match purpose {
        LeasePurpose::Fix => inputs.findings_to_fix.as_slice(),
        LeasePurpose::Review => inputs.rejections.as_slice(),
        LeasePurpose::Implementation | LeasePurpose::Strategy => &[],
    };
    for run in findings {
        if run.runner_id.as_deref() == Some(inputs.claiming_runner.as_str()) {
            continue;
        }
        pieces.push(Piece {
            kind: ContentKind::ReviewFindings,
            task_id: Some(task.clone()),
            revision: run.run_id.clone(),
            author: run.owner.clone(),
            written_during_run: false,
        });
    }

    if let Some(base) = &inputs.base_commit {
        for owner in &base.owners {
            pieces.push(Piece {
                kind: ContentKind::BaseCommit,
                task_id: Some(base.task_id.clone()),
                revision: base.commit.clone(),
                author: owner.clone(),
                written_during_run: false,
            });
        }
    }

    pieces
}

fn revisioned(kind: ContentKind, task: Option<&TaskId>, revision: &Revision) -> Piece {
    Piece {
        kind,
        task_id: task.cloned(),
        revision: revision.revision.to_string(),
        author: revision.author.clone(),
        written_during_run: revision.written_during_run,
    }
}

/// One `acceptances` row, as consent compares it: `(kind, task, revision)`,
/// for the owner whose list it was read from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Accepted {
    pub kind: ContentKind,
    pub task_id: Option<TaskId>,
    pub revision: String,
}

impl Accepted {
    fn covers(&self, piece: &Piece) -> bool {
        self.kind == piece.kind && self.task_id == piece.task_id && self.revision == piece.revision
    }
}

/// Why a piece does not consent, which decides the sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MissingReason {
    /// Someone else wrote it, and the owner neither accepted nor trusts it.
    NotAccepted,
    /// Its author's account is gone, and nobody trusts a former member.
    FormerMember,
    /// ADR-0032 point 6: only an acceptance of exactly this revision counts.
    WrittenDuringRun,
}

/// Whether the owner consents to one piece.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consent {
    Consents,
    Missing { piece: Piece, reason: MissingReason },
}

/// Whether `owner` consents to `piece`, given the owner's acceptances and
/// their trust list for the task's team, in this order:
///
/// 1. A piece written during a run: only an acceptance of exactly `(owner,
///    kind, task, revision)` counts, **its author's own runners included**.
///    A plan Bob's credentials wrote while Bob's runner ran Alice's plan must
///    not pass Bob's runners on authorship (point 6).
/// 2. The owner is the author.
/// 3. An acceptance of exactly that revision.
/// 4. The owner trusts the author. A former member is never trusted.
/// 5. Otherwise missing.
pub fn consents(owner: &str, piece: &Piece, accepted: &[Accepted], trusted: &[UserId]) -> Consent {
    let is_accepted = accepted.iter().any(|row| row.covers(piece));
    let missing = |reason| Consent::Missing {
        piece: piece.clone(),
        reason,
    };

    if piece.written_during_run {
        return if is_accepted {
            Consent::Consents
        } else {
            missing(MissingReason::WrittenDuringRun)
        };
    }
    if piece.author.as_deref() == Some(owner) || is_accepted {
        return Consent::Consents;
    }
    match &piece.author {
        Some(author) if trusted.contains(author) => Consent::Consents,
        Some(_) => missing(MissingReason::NotAccepted),
        None => missing(MissingReason::FormerMember),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const TASK: &str = "task-1";
    const ME: &str = "runner-me";
    const OTHER: &str = "runner-other";

    fn revision(number: i64, author: &str) -> Option<Revision> {
        Some(Revision {
            revision: number,
            author: Some(author.to_string()),
            written_during_run: false,
        })
    }

    fn run(id: &str, runner: &str, owner: &str) -> RunOnRunner {
        RunOnRunner {
            run_id: id.to_string(),
            runner_id: Some(runner.to_string()),
            owner: Some(owner.to_string()),
        }
    }

    /// A task with every kind of content, findings from both runners, and a
    /// dependency commit.
    fn everything() -> PieceInputs {
        PieceInputs {
            task_id: TASK.to_string(),
            claiming_runner: ME.to_string(),
            plan: revision(4, "alice"),
            base_instructions: revision(2, "olga"),
            task_review_instructions: revision(3, "rita"),
            team_review_instructions: revision(5, "olga"),
            findings_to_fix: vec![
                run("review-mine", ME, "bob"),
                run("review-theirs", OTHER, "carol"),
            ],
            rejections: vec![
                run("fix-mine", ME, "bob"),
                run("fix-theirs", OTHER, "carol"),
            ],
            base_commit: Some(BaseCommitInput {
                task_id: "dependency".to_string(),
                commit: "abc123".to_string(),
                owners: vec![Some("alice".to_string())],
            }),
        }
    }

    fn piece(kind: ContentKind, task: Option<&str>, revision: &str, author: &str) -> Piece {
        Piece {
            kind,
            task_id: task.map(str::to_string),
            revision: revision.to_string(),
            author: Some(author.to_string()),
            written_during_run: false,
        }
    }

    fn plan() -> Piece {
        piece(ContentKind::Plan, Some(TASK), "4", "alice")
    }

    fn base_commit() -> Piece {
        piece(
            ContentKind::BaseCommit,
            Some("dependency"),
            "abc123",
            "alice",
        )
    }

    /// An empty plan, a blank override, a base on the default branch and no
    /// findings: what each purpose's list must lose.
    fn nothing_optional() -> PieceInputs {
        PieceInputs {
            plan: None,
            task_review_instructions: None,
            team_review_instructions: None,
            base_instructions: None,
            findings_to_fix: Vec::new(),
            rejections: Vec::new(),
            base_commit: None,
            ..everything()
        }
    }

    #[test]
    fn an_implementation_run_needs_the_plan_the_base_instructions_and_the_base_commit() {
        assert_eq!(
            pieces_for(LeasePurpose::Implementation, &everything()),
            vec![
                plan(),
                piece(ContentKind::BaseInstructions, None, "2", "olga"),
                base_commit(),
            ]
        );
        assert_eq!(
            pieces_for(LeasePurpose::Implementation, &nothing_optional()),
            Vec::new()
        );
    }

    #[test]
    fn a_planner_run_needs_no_base_instructions() {
        assert_eq!(
            pieces_for(LeasePurpose::Strategy, &everything()),
            vec![plan(), base_commit()]
        );
        assert_eq!(
            pieces_for(LeasePurpose::Strategy, &nothing_optional()),
            Vec::new()
        );
    }

    #[test]
    fn a_review_run_reads_the_override_and_rejections_from_another_runner() {
        assert_eq!(
            pieces_for(LeasePurpose::Review, &everything()),
            vec![
                plan(),
                piece(ContentKind::TaskReviewInstructions, Some(TASK), "3", "rita"),
                piece(
                    ContentKind::ReviewFindings,
                    Some(TASK),
                    "fix-theirs",
                    "carol"
                ),
                base_commit(),
            ]
        );
        // A blank override falls through to the team's text.
        let inherited = PieceInputs {
            task_review_instructions: None,
            ..everything()
        };
        assert_eq!(
            pieces_for(LeasePurpose::Review, &inherited)[1],
            piece(ContentKind::ReviewInstructions, None, "5", "olga")
        );
        assert_eq!(
            pieces_for(LeasePurpose::Review, &nothing_optional()),
            Vec::new()
        );
    }

    #[test]
    fn a_fix_run_on_the_runner_that_reviewed_needs_no_consent_to_its_own_findings() {
        assert_eq!(
            pieces_for(LeasePurpose::Fix, &everything()),
            vec![
                plan(),
                piece(ContentKind::BaseInstructions, None, "2", "olga"),
                piece(
                    ContentKind::ReviewFindings,
                    Some(TASK),
                    "review-theirs",
                    "carol"
                ),
                base_commit(),
            ]
        );
        assert_eq!(
            pieces_for(LeasePurpose::Fix, &nothing_optional()),
            Vec::new()
        );
    }

    #[test]
    fn a_base_commit_built_by_two_owners_runners_needs_consent_to_both() {
        let inputs = PieceInputs {
            base_commit: Some(BaseCommitInput {
                task_id: "dependency".to_string(),
                commit: "abc123".to_string(),
                owners: vec![Some("alice".to_string()), Some("bob".to_string())],
            }),
            ..nothing_optional()
        };

        let pieces = pieces_for(LeasePurpose::Implementation, &inputs);
        assert_eq!(
            pieces,
            vec![
                base_commit(),
                piece(ContentKind::BaseCommit, Some("dependency"), "abc123", "bob"),
            ]
        );

        // Trusting one owner leaves the other's work waiting; one acceptance
        // of the commit covers both.
        let trusted = vec!["alice".to_string()];
        assert_eq!(
            consents("carol", &pieces[0], &[], &trusted),
            Consent::Consents
        );
        assert!(matches!(
            consents("carol", &pieces[1], &[], &trusted),
            Consent::Missing {
                reason: MissingReason::NotAccepted,
                ..
            }
        ));
        let accepted = vec![Accepted {
            kind: ContentKind::BaseCommit,
            task_id: Some("dependency".to_string()),
            revision: "abc123".to_string(),
        }];
        for piece in &pieces {
            assert_eq!(consents("carol", piece, &accepted, &[]), Consent::Consents);
        }
    }

    #[test]
    fn the_author_consents_to_their_own_revision() {
        assert_eq!(consents("alice", &plan(), &[], &[]), Consent::Consents);
    }

    #[test]
    fn an_acceptance_covers_exactly_one_revision() {
        let accepted = vec![Accepted {
            kind: ContentKind::Plan,
            task_id: Some(TASK.to_string()),
            revision: "4".to_string(),
        }];
        assert_eq!(consents("bob", &plan(), &accepted, &[]), Consent::Consents);

        let next = Piece {
            revision: "5".to_string(),
            ..plan()
        };
        assert_eq!(
            consents("bob", &next, &accepted, &[]),
            Consent::Missing {
                piece: next.clone(),
                reason: MissingReason::NotAccepted,
            }
        );
        // The same revision of another task is another piece.
        let elsewhere = Piece {
            task_id: Some("task-2".to_string()),
            ..plan()
        };
        assert!(matches!(
            consents("bob", &elsewhere, &accepted, &[]),
            Consent::Missing { .. }
        ));
    }

    #[test]
    fn trusting_an_author_consents_to_their_next_revision() {
        let trusted = vec!["alice".to_string()];
        let next = Piece {
            revision: "5".to_string(),
            ..plan()
        };
        assert_eq!(consents("bob", &next, &[], &trusted), Consent::Consents);
    }

    #[test]
    fn a_former_member_is_never_trusted() {
        let gone = Piece {
            author: None,
            ..plan()
        };
        assert_eq!(
            consents("bob", &gone, &[], &["alice".to_string()]),
            Consent::Missing {
                piece: gone.clone(),
                reason: MissingReason::FormerMember,
            }
        );
    }

    #[test]
    fn a_revision_written_during_a_run_needs_an_acceptance_even_from_its_author() {
        let laundered = Piece {
            author: Some("bob".to_string()),
            written_during_run: true,
            ..plan()
        };
        for owner in ["bob", "carol"] {
            assert_eq!(
                consents(owner, &laundered, &[], &["bob".to_string()]),
                Consent::Missing {
                    piece: laundered.clone(),
                    reason: MissingReason::WrittenDuringRun,
                },
                "{owner}'s runner"
            );
        }
        let accepted = vec![Accepted {
            kind: ContentKind::Plan,
            task_id: Some(TASK.to_string()),
            revision: "4".to_string(),
        }];
        assert_eq!(
            consents("bob", &laundered, &accepted, &[]),
            Consent::Consents
        );
    }
}
