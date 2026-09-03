//! What a task's branch is created from (ADR-0008's branch chaining).
//!
//! Its own file rather than a longer [`super`]: `mod.rs` is already seven
//! hundred lines of worktree mechanics, and this is the one thing in it that is
//! a *policy* — the answer changes when another task moves column, and nothing
//! else in that file has that property.
//!
//! # The rule, in order
//!
//! 1. **No dependencies** → the repository's configured default branch. This is
//!    the whole of what task 007 shipped and remains the common case.
//! 2. **One or more** → the branch of the highest-ranked *satisfied* dependency
//!    that actually has a branch, where the order is
//!    [`crate::db::BoardColumn::board_rank`] first and then ascending `position`
//!    ([`tasks::dependencies_of`] owns that comparator, so the base a task
//!    chains from and the blocker its card names are the same row).
//! 3. **A dependency with no branch cannot be a base.** A task that has never
//!    run has nothing to branch from — `tasks.branch` is NULL — and
//!    `git worktree add <path> -b <new> <base>` needs a real committish. It
//!    falls through to the default branch, and says so in the warning rather
//!    than silently.
//! 4. **Every other dependency is named in a warning.** ADR-0008: "the others
//!    are surfaced as an explicit warning that the user should either merge
//!    them or serialize the work."
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

use crate::db::{Repository, Task};
use crate::error::Result;
use crate::tasks;
use crate::ServiceContext;

/// What a worktree is built on, and what the user should know about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedBaseRef {
    /// A local branch name, never `origin/<branch>` — see [`resolve`].
    pub base_ref: String,
    /// ADR-0008's explicit warning, or `None` when there is nothing to say.
    /// One sentence, rendered verbatim: seam-contract D8 puts the specificity
    /// in the message.
    pub warning: Option<String>,
}

/// Resolves the base ref for `task`, following ADR-0008.
///
/// A **local** branch name, never `origin/<branch>`, which is task 007's
/// choice and still holds for a dependency's branch: a dependency's work only
/// exists locally until somebody pushes it, so `origin/<dependency-branch>`
/// would not resolve at all, and consulting the remote would give a different
/// answer after every fetch — a branch that appeared to gain and lose commits
/// nobody wrote.
pub(super) async fn resolve(
    ctx: &ServiceContext,
    task: &Task,
    repository: &Repository,
) -> Result<ResolvedBaseRef> {
    let dependencies = tasks::dependencies_of(ctx, &task.id).await?;
    Ok(choose(&dependencies, &repository.default_branch))
}

/// The pure half: given a task's dependencies in ADR-0008's order and the
/// repository's default branch, which base ref and which warning.
///
/// Separated from the read for the reason `dependencies::find_path` gives at
/// its own split — the interesting half is a decision over rows, and it is
/// worth exhausting without a pool.
fn choose(dependencies: &[Task], default_branch: &str) -> ResolvedBaseRef {
    if dependencies.is_empty() {
        return ResolvedBaseRef {
            base_ref: default_branch.to_string(),
            warning: None,
        };
    }

    let chosen = dependencies
        .iter()
        .find(|dependency| dependency.column.satisfies_a_dependency() && has_branch(dependency));

    let base_ref = match chosen {
        Some(dependency) => dependency.branch.clone().expect("has_branch just said so"),
        None => default_branch.to_string(),
    };

    let warning = warn_about(dependencies, chosen, &base_ref);
    ResolvedBaseRef { base_ref, warning }
}

/// A branch that is recorded and non-blank. `tasks.branch` is NULL until
/// `worktree::prepare` writes it, and a blank string is the same "nothing to
/// branch from" as a NULL — `plan_is_present` makes the identical judgement
/// about `tasks.plan` and for the same reason: two spellings of absence is one
/// too many.
fn has_branch(task: &Task) -> bool {
    task.branch
        .as_deref()
        .is_some_and(|branch| !branch.trim().is_empty())
}

/// ADR-0008's warning: what is *not* in the base the task is about to be built
/// on.
///
/// Named individually rather than counted, because the remedy the ADR gives —
/// "merge them or serialize the work" — is something the user performs on a
/// specific branch. A count tells them a problem exists and not which cards it
/// is about.
fn warn_about(dependencies: &[Task], chosen: Option<&Task>, base_ref: &str) -> Option<String> {
    let chosen_id = chosen.map(|dependency| dependency.id.as_str());
    let others: Vec<&Task> = dependencies
        .iter()
        .filter(|dependency| Some(dependency.id.as_str()) != chosen_id)
        .collect();
    if others.is_empty() {
        return None;
    }

    let names = others
        .iter()
        .map(|dependency| format!("\"{}\"", dependency.title))
        .collect::<Vec<_>>()
        .join(", ");

    // Two sentences for two situations, because the remedy differs. With a
    // dependency chosen, the others are simply not merged into the base. With
    // none chosen the task is branching off the default branch as if it had no
    // dependencies at all — the surprising case, so it gets the reason spelled
    // out rather than left to be inferred from the branch name.
    //
    // Whole clauses rather than a pluralized noun, for the reason `repo::remove`
    // gives at its own count: English inflects the verb as well as the noun.
    let one = others.len() == 1;
    Some(match chosen {
        Some(dependency) => {
            let clause = if one {
                format!("{names} is also a dependency and is not in that base")
            } else {
                format!("{names} are also dependencies and are not in that base")
            };
            format!(
                "This task branches from \"{title}\" ({base_ref}). {clause} — \
                 merge into it what you need, or run this task again once the rest \
                 have landed.",
                title = dependency.title,
            )
        }
        None => {
            let clause = if one {
                format!("{names} has not produced one")
            } else {
                format!("{names} have not produced one")
            };
            format!(
                "This task branches from {base_ref}: none of its dependencies has a \
                 branch to build on yet. {clause} — a dependency that has never run \
                 cannot be a base."
            )
        }
    })
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
            ResolvedBaseRef {
                base_ref: "main".to_string(),
                warning: None,
            }
        );
    }

    #[test]
    fn one_satisfied_dependency_is_the_base_and_needs_no_warning() {
        let dependencies = [dependency(
            "a",
            BoardColumn::InReview,
            1.0,
            Some("rimaia/a"),
        )];

        assert_eq!(
            choose(&dependencies, "main"),
            ResolvedBaseRef {
                base_ref: "rimaia/a".to_string(),
                warning: None,
            }
        );
    }

    #[test]
    fn an_unsatisfied_dependency_is_never_a_base_even_when_it_has_a_branch() {
        // A failed run leaves the card in `ready` with a branch holding
        // whatever it committed (ADR-0007's failure rule). Building the next
        // task on that is building on work nobody accepted — ADR-0008 gates
        // chaining on the *column*, not on the branch existing.
        let dependencies = [dependency("a", BoardColumn::Ready, 1.0, Some("rimaia/a"))];

        let resolved = choose(&dependencies, "main");

        assert_eq!(resolved.base_ref, "main");
        assert!(
            resolved
                .warning
                .as_deref()
                .is_some_and(|warning| warning.contains("\"a\"")),
            "the warning must name the dependency that is not in the base: {:?}",
            resolved.warning,
        );
    }

    #[test]
    fn a_dependency_that_has_never_run_cannot_be_a_base() {
        // Satisfied — the user implemented it by hand and dragged it to `done`
        // — but there is no branch, so there is nothing to branch from.
        let dependencies = [dependency("a", BoardColumn::Done, 1.0, None)];

        let resolved = choose(&dependencies, "main");

        assert_eq!(resolved.base_ref, "main");
        assert_eq!(
            resolved.warning.as_deref(),
            Some(
                "This task branches from main: none of its dependencies has a branch to \
                 build on yet. \"a\" has not produced one — a dependency that has never \
                 run cannot be a base."
            ),
        );
    }

    #[test]
    fn a_blank_branch_is_the_same_as_no_branch() {
        // `tasks.branch` is written only by `write_worktree_columns`, but
        // ADR-0003 supports a user editing the file with the `sqlite3` CLI, and
        // `git worktree add … ''` is not an error message anybody can act on.
        let dependencies = [dependency("a", BoardColumn::Done, 1.0, Some("  "))];

        assert_eq!(choose(&dependencies, "main").base_ref, "main");
    }

    #[test]
    fn two_dependencies_base_off_the_first_in_order_and_warn_about_the_other() {
        // Already sorted by `dependencies_of`; this asserts `choose` takes the
        // head rather than re-deciding.
        let dependencies = [
            dependency("a", BoardColumn::InReview, 1.0, Some("rimaia/a")),
            dependency("b", BoardColumn::InReview, 2.0, Some("rimaia/b")),
        ];

        let resolved = choose(&dependencies, "main");

        assert_eq!(resolved.base_ref, "rimaia/a");
        assert_eq!(
            resolved.warning.as_deref(),
            Some(
                "This task branches from \"a\" (rimaia/a). \"b\" is also a dependency and \
                 is not in that base — merge into it what you need, or run this task \
                 again once the rest have landed."
            ),
        );
    }

    #[test]
    fn the_first_satisfied_dependency_with_a_branch_wins_over_earlier_ones_without() {
        // Order is not "the first row"; it is the first row that can actually
        // be a base. `a` is satisfied but never ran, `b` is satisfied and has a
        // branch — so `b` is the base and `a` is warned about.
        let dependencies = [
            dependency("a", BoardColumn::InReview, 1.0, None),
            dependency("b", BoardColumn::InReview, 2.0, Some("rimaia/b")),
        ];

        let resolved = choose(&dependencies, "main");

        assert_eq!(resolved.base_ref, "rimaia/b");
        assert!(resolved
            .warning
            .as_deref()
            .is_some_and(|warning| warning.contains("\"a\"")));
    }

    /// One dependency row, with everything the rule does not read left boring.
    fn dependency(title: &str, column: BoardColumn, position: f64, branch: Option<&str>) -> Task {
        Task {
            id: format!("dependency-{title}"),
            repository_id: "repository".to_string(),
            title: title.to_string(),
            plan: None,
            extra_instructions: None,
            column,
            position,
            run_state: RunState::Idle,
            branch: branch.map(str::to_string),
            worktree_path: None,
            strategy_mode: StrategyMode::Default,
            model: None,
            effort: None,
            strategy_plan: None,
            strategy_source: None,
            strategy_updated_at: None,
            created_at: test_epoch(),
            updated_at: test_epoch(),
            source: MutationSource::Ui,
        }
    }
}
