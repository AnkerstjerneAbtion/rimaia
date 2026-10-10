//! What a task's branch is created from (ADR-0008's branch chaining, as
//! ADR-0033 point 5 amends it).
//!
//! Its own file rather than a longer [`super`]: `mod.rs` is already seven
//! hundred lines of worktree mechanics, and this is the one thing in it that is
//! a *policy* — the answer changes when another task moves column, and nothing
//! else in that file has that property.
//!
//! The board decides it, not the runner (seam-contract D31 point 6). Since
//! task 044 [`resolve`] is called where `board::service` builds a
//! `RunContext`, which carries the answer to the runner as `RunContext::base`;
//! `worktree::prepare` creates the worktree from that and reads no dependency
//! graph itself. `worktree::status` and `worktree::diff_summary` still call it
//! for their fresh-resolution fallback.
//!
//! # The rule, in order
//!
//! 1. **No dependencies** → the repository's configured default branch. This is
//!    the whole of what task 007 shipped and remains the common case.
//! 2. **One or more** → the **commit** the highest-ranked *satisfied*
//!    dependency's latest successful implementation or fix run ended on
//!    (`runs.head_sha`, through [`runs::latest_successful_head`]), where the
//!    order is [`crate::db::BoardColumn::board_rank`] first and then ascending
//!    `position` ([`tasks::dependencies_of`] owns that comparator, so the base
//!    a task chains from and the blocker its card names are the same row).
//!    Satisfaction is still the column alone (ADR-0008 amendment point 2).
//! 3. **A satisfied dependency with no successful head cannot be a base.** One
//!    dragged to `done` by hand, or one whose only successful runs predate
//!    task 033's `head_sha`, has no commit anyone verified. It falls through to
//!    the next candidate and then to the default branch, and says so in the
//!    warning rather than silently.
//! 4. **Every other dependency is named in a warning.** ADR-0008: "the others
//!    are surfaced as an explicit warning that the user should either merge
//!    them or serialize the work."
//!
//! A commit rather than a branch name because a branch moves. If A is retried
//! after B has started, A's branch points at A's newer work while B was built
//! on the older commit, and only the commit can record that. It is also the
//! one name two machines share: B's runner may never have seen A's branch.
//!
//! **Implementation and fix rows only** (D29 point 5, amended 2026-10-04). A
//! succeeded review that moved `HEAD` made commits nobody reviewed, and a
//! failed fix is skipped even if it committed.
//!
//! # What `base_ref` means now
//!
//! [`RunBase::base_ref`] is a **label**: the chosen dependency's `tasks.branch`
//! when one is recorded, the full commit when that branch has been cleaned up
//! (task 016, D20) or is blank, and the default branch when no dependency is
//! chosen. It is what `runs.base_ref` records and the panel shows. **`base_sha`
//! is authoritative**, and may be behind the label's tip: a fix that failed
//! after committing, or a commit added to the branch by hand, moves the branch
//! and not the base. D28 part 6's comment on task 033's migration ("`base_sha`
//! is what that name resolved to") is read with that refinement, which D29's
//! 2026-10-04 amendment also records.
//!
//! # Why the *satisfied* pair ranks `in_review` before `done`
//!
//! Both satisfy a dependency, so both can be a base, and they are ranked in
//! board order — `in_review` (2) before `done` (3). That is deliberate and it
//! is the case `board_column ASC` would get backwards, since `'done'` sorts
//! first alphabetically. A card in `in_review` has just been produced by a run
//! and its branch is live and unmerged, which is exactly the stack ADR-0008
//! describes; a card in `done` is one the user has finished with, whose branch
//! may well already be merged into the default branch and deleted. Chaining
//! onto the live one is the answer that keeps the stack reviewable in order.

use crate::board::{BaseDependency, RunBase};
use crate::db::{Repository, Task};
use crate::error::Result;
use crate::runs::{self, SuccessfulHead};
use crate::tasks;
use crate::ServiceContext;

/// Resolves what `task` branches from, following ADR-0033 point 5.
///
/// Reads [`tasks::dependencies_of`] and then one
/// [`runs::latest_successful_head`] per dependency: a task has a handful of
/// dependencies at most, and one query per edge keeps D29's single function
/// the only place "successful head" is defined. Every read is under `ctx`'s
/// scope.
pub(crate) async fn resolve(
    ctx: &ServiceContext,
    task: &Task,
    repository: &Repository,
) -> Result<RunBase> {
    let mut dependencies = Vec::new();
    for dependency in tasks::dependencies_of(ctx, &task.id).await? {
        let head = runs::latest_successful_head(ctx, &dependency.id).await?;
        dependencies.push((dependency, head));
    }
    Ok(choose(&dependencies, &repository.default_branch))
}

/// The pure half: given a task's dependencies in ADR-0008's order, each with
/// its latest successful head, and the repository's default branch, which base
/// and which warning.
///
/// Separated from the read for the reason `dependencies::find_path` gives at
/// its own split — the interesting half is a decision over rows, and it is
/// worth exhausting without a pool. It runs no git.
fn choose(dependencies: &[(Task, Option<SuccessfulHead>)], default_branch: &str) -> RunBase {
    let chosen = dependencies.iter().find_map(|(dependency, head)| {
        let head = head.as_ref()?;
        dependency
            .column
            .satisfies_a_dependency()
            .then_some((dependency, head))
    });

    let Some((dependency, head)) = chosen else {
        return RunBase {
            base_ref: default_branch.to_string(),
            dependency: None,
            warning: warn_without_base(dependencies, default_branch),
        };
    };

    let base_ref = recorded_branch(dependency).unwrap_or_else(|| head.head_sha.clone());
    let warning = warn_about_others(dependencies, dependency, &base_ref);
    RunBase {
        base_ref,
        dependency: Some(BaseDependency {
            task_id: dependency.id.clone(),
            title: dependency.title.clone(),
            run_id: head.run_id.clone(),
            commit: head.head_sha.clone(),
        }),
        warning,
    }
}

/// The dependency's branch when it is recorded and non-blank. `tasks.branch`
/// is NULL until `worktree::prepare` writes it and again after task 016's
/// cleanup deletes the branch, and a blank string is the same absence:
/// `plan_is_present` makes the identical judgement about `tasks.plan`. The
/// board never runs git, so this is all "cleaned up" can mean here.
fn recorded_branch(task: &Task) -> Option<String> {
    task.branch
        .as_deref()
        .filter(|branch| !branch.trim().is_empty())
        .map(str::to_string)
}

/// ADR-0008's warning with a dependency chosen: what is *not* in the base the
/// task is about to be built on.
///
/// Named individually rather than counted, because the remedy the ADR gives —
/// "merge them or serialize the work" — is something the user performs on a
/// specific branch. A count tells them a problem exists and not which cards it
/// is about.
fn warn_about_others(
    dependencies: &[(Task, Option<SuccessfulHead>)],
    chosen: &Task,
    base_ref: &str,
) -> Option<String> {
    let others: Vec<&Task> = dependencies
        .iter()
        .map(|(dependency, _)| dependency)
        .filter(|dependency| dependency.id != chosen.id)
        .collect();
    if others.is_empty() {
        return None;
    }

    // Whole clauses rather than a pluralized noun, for the reason `repo::remove`
    // gives at its own count: English inflects the verb as well as the noun.
    let names = quoted(&others);
    let clause = if others.len() == 1 {
        format!("{names} is also a dependency and is not in that base")
    } else {
        format!("{names} are also dependencies and are not in that base")
    };
    Some(format!(
        "This task branches from \"{title}\" ({base_ref}). {clause} — merge into it what \
         you need, or run this task again once the rest have landed.",
        title = chosen.title,
    ))
}

/// ADR-0008's warning with no dependency chosen: the task is branching off the
/// default branch as if it had no dependencies at all, which is the surprising
/// case, so each dependency is named under the reason it cannot be a base.
/// The two reasons have different remedies: an unsatisfied dependency needs
/// to reach review, a satisfied one with no successful run needs a run.
fn warn_without_base(
    dependencies: &[(Task, Option<SuccessfulHead>)],
    default_branch: &str,
) -> Option<String> {
    if dependencies.is_empty() {
        return None;
    }

    // With nothing chosen, every satisfied dependency is one with no head.
    let (satisfied, unsatisfied): (Vec<&Task>, Vec<&Task>) = dependencies
        .iter()
        .map(|(dependency, _)| dependency)
        .partition(|dependency| dependency.column.satisfies_a_dependency());

    let clauses: Vec<String> = [
        clause(
            &unsatisfied,
            "is not in review or done",
            "are not in review or done",
        ),
        clause(
            &satisfied,
            "has no successful run to build on",
            "have no successful run to build on",
        ),
    ]
    .into_iter()
    .flatten()
    .collect();

    Some(format!(
        "This task branches from {default_branch}: none of its dependencies can be built on \
         yet. {}.",
        clauses.join(", and "),
    ))
}

/// One clause of [`warn_without_base`], or `None` when it names nobody.
fn clause(tasks: &[&Task], singular: &str, plural: &str) -> Option<String> {
    match tasks.len() {
        0 => None,
        1 => Some(format!("{} {singular}", quoted(tasks))),
        _ => Some(format!("{} {plural}", quoted(tasks))),
    }
}

fn quoted(tasks: &[&Task]) -> String {
    tasks
        .iter()
        .map(|task| format!("\"{}\"", task.title))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{BoardColumn, MutationSource, RunState, StrategyMode};
    use crate::testing::test_epoch;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_task_with_no_dependencies_branches_from_the_default_branch() {
        assert_eq!(
            choose(&[], "main"),
            RunBase {
                base_ref: "main".to_string(),
                dependency: None,
                warning: None,
            }
        );
    }

    #[test]
    fn one_satisfied_dependency_with_a_successful_head_is_the_base_and_needs_no_warning() {
        let dependencies = [(
            dependency("a", BoardColumn::InReview, Some("rimaia/a")),
            Some(head("a")),
        )];

        assert_eq!(
            choose(&dependencies, "main"),
            RunBase {
                base_ref: "rimaia/a".to_string(),
                dependency: Some(BaseDependency {
                    task_id: "dependency-a".to_string(),
                    title: "a".to_string(),
                    run_id: "run-a".to_string(),
                    commit: sha("a"),
                }),
                warning: None,
            }
        );
    }

    #[test]
    fn an_unsatisfied_dependency_is_never_a_base_even_with_a_successful_head() {
        // A run succeeded and the user dragged the card back to `ready` for
        // another go. ADR-0008 gates chaining on the *column*, not on a run.
        let dependencies = [(
            dependency("a", BoardColumn::Ready, Some("rimaia/a")),
            Some(head("a")),
        )];

        assert_eq!(
            choose(&dependencies, "main"),
            RunBase {
                base_ref: "main".to_string(),
                dependency: None,
                warning: Some(
                    "This task branches from main: none of its dependencies can be built on \
                     yet. \"a\" is not in review or done."
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn a_satisfied_dependency_with_no_successful_head_cannot_be_a_base() {
        // Satisfied — implemented by hand and dragged to `done`, or with a
        // branch whose runs never succeeded — but no commit anyone verified.
        let dependencies = [(dependency("a", BoardColumn::Done, Some("rimaia/a")), None)];

        assert_eq!(
            choose(&dependencies, "main"),
            RunBase {
                base_ref: "main".to_string(),
                dependency: None,
                warning: Some(
                    "This task branches from main: none of its dependencies can be built on \
                     yet. \"a\" has no successful run to build on."
                        .to_string()
                ),
            }
        );
    }

    #[test]
    fn a_no_base_warning_names_each_dependency_under_its_reason() {
        // Interleaved on purpose: each clause keeps `dependencies_of` order.
        let dependencies = [
            (dependency("c", BoardColumn::InReview, None), None),
            (dependency("a", BoardColumn::Ready, None), Some(head("a"))),
            (dependency("d", BoardColumn::Done, Some("rimaia/d")), None),
            (dependency("b", BoardColumn::NotReady, None), None),
        ];

        assert_eq!(
            choose(&dependencies, "main").warning.as_deref(),
            Some(
                "This task branches from main: none of its dependencies can be built on yet. \
                 \"a\", \"b\" are not in review or done, and \"c\", \"d\" have no successful \
                 run to build on."
            ),
        );
    }

    #[test]
    fn the_first_satisfied_dependency_with_a_head_wins_over_earlier_ones_without() {
        // Order is not "the first row"; it is the first row that can actually
        // be a base. `a` is satisfied but has no successful run, `b` has one.
        let dependencies = [
            (
                dependency("a", BoardColumn::InReview, Some("rimaia/a")),
                None,
            ),
            (
                dependency("b", BoardColumn::InReview, Some("rimaia/b")),
                Some(head("b")),
            ),
        ];

        let base = choose(&dependencies, "main");

        assert_eq!(base.base_ref, "rimaia/b");
        assert_eq!(
            base.dependency.map(|dependency| dependency.commit),
            Some(sha("b"))
        );
        assert_eq!(
            base.warning.as_deref(),
            Some(
                "This task branches from \"b\" (rimaia/b). \"a\" is also a dependency and is \
                 not in that base — merge into it what you need, or run this task again once \
                 the rest have landed."
            ),
        );
    }

    #[test]
    fn two_dependencies_base_off_the_first_and_warn_about_the_other() {
        // Already sorted by `dependencies_of`; this asserts `choose` takes the
        // head rather than re-deciding.
        let dependencies = [
            (
                dependency("a", BoardColumn::InReview, Some("rimaia/a")),
                Some(head("a")),
            ),
            (
                dependency("b", BoardColumn::InReview, Some("rimaia/b")),
                Some(head("b")),
            ),
        ];

        let base = choose(&dependencies, "main");

        assert_eq!(base.base_ref, "rimaia/a");
        assert_eq!(
            base.warning.as_deref(),
            Some(
                "This task branches from \"a\" (rimaia/a). \"b\" is also a dependency and is \
                 not in that base — merge into it what you need, or run this task again once \
                 the rest have landed."
            ),
        );
    }

    #[test]
    fn a_dependency_whose_branch_is_gone_is_named_by_its_commit() {
        // Task 016's cleanup clears `tasks.branch`; the commit is still the
        // base, and a label nobody can resolve is worse than a hash.
        let dependencies = [(dependency("a", BoardColumn::Done, None), Some(head("a")))];

        let base = choose(&dependencies, "main");

        let commit = base
            .dependency
            .as_ref()
            .map(|dependency| &dependency.commit);
        assert_eq!(Some(&base.base_ref), commit);
        assert_eq!(base.base_ref, sha("a"));
    }

    #[test]
    fn a_blank_branch_is_named_by_its_commit_like_a_missing_one() {
        // `tasks.branch` is written only through the board, but ADR-0003
        // supports a user editing the file with the `sqlite3` CLI.
        let dependencies = [(
            dependency("a", BoardColumn::Done, Some("  ")),
            Some(head("a")),
        )];

        let base = choose(&dependencies, "main");

        assert_eq!(base.base_ref, sha("a"));
        assert_ne!(base.base_ref, "main");
    }

    /// A full-length, recognisably fake commit for `title`.
    fn sha(title: &str) -> String {
        format!("{title:0>40}")
    }

    fn head(title: &str) -> SuccessfulHead {
        SuccessfulHead {
            run_id: format!("run-{title}"),
            head_sha: sha(title),
        }
    }

    /// One dependency row, with everything the rule does not read left boring.
    fn dependency(title: &str, column: BoardColumn, branch: Option<&str>) -> Task {
        Task {
            id: format!("dependency-{title}"),
            repository_id: "repository".to_string(),
            title: title.to_string(),
            plan: None,
            extra_instructions: None,
            column,
            position: 1.0,
            run_state: RunState::Idle,
            branch: branch.map(str::to_string),
            strategy_mode: StrategyMode::Default,
            model: None,
            effort: None,
            strategy_plan: None,
            strategy_source: None,
            strategy_updated_at: None,
            created_at: test_epoch(),
            updated_at: test_epoch(),
            source: MutationSource::Ui,
            archived_at: None,
        }
    }
}
