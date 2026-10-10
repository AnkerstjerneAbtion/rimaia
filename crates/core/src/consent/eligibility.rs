//! Which runners may take a task at all (ADR-0032 point 2).
//!
//! Pure. [`super`] reads the facts inside the asking transaction.

use serde::{Deserialize, Serialize};

use crate::events::TeamId;

/// A runner's eligibility policy, in the spelling of `runners.eligibility`'s
/// `CHECK`.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    sqlx::Type,
    schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[sqlx(rename_all = "snake_case")]
pub enum RunnerEligibility {
    /// Tasks assigned to the runner's owner. The default.
    #[default]
    Assigned,
    /// The above first, then unassigned tasks in the teams the owner picked.
    AssignedThenPool,
}

/// Why a runner may not take a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Never a policy option: moving work to another person is reassigning
    /// the card.
    AssignedToSomeoneElse,
    /// In the pool of a team this runner does not take pool work from.
    Unassigned,
}

/// Whether a runner may take a task, and on which footing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eligibility {
    /// The owner's own task: assigned to them, or unassigned in their personal
    /// team. These come first in a runner's queue.
    Assigned,
    /// An unassigned task in a team the runner takes pool work from.
    Pool,
    NotEligible(Reason),
}

/// The facts about a task that eligibility reads.
#[derive(Debug, Clone, Copy)]
pub struct TaskFacts<'a> {
    pub assignee_id: Option<&'a str>,
    pub team_id: &'a str,
    /// The team's `personal_user_id`: whose personal team it is, if anyone's.
    pub personal_owner: Option<&'a str>,
}

/// The facts about a runner that eligibility reads.
#[derive(Debug, Clone, Copy)]
pub struct RunnerFacts<'a> {
    pub owner: &'a str,
    pub policy: RunnerEligibility,
}

/// ADR-0032 point 2, in order:
///
/// - assigned to the runner's owner: [`Assigned`](Eligibility::Assigned);
/// - assigned to anyone else: never, whatever the policy;
/// - unassigned in the owner's **personal** team: `Assigned`, because there
///   every task is the owner's own (which is why the migration assigns
///   nothing);
/// - unassigned, `assigned_then_pool`, in a team the runner opted into:
///   [`Pool`](Eligibility::Pool);
/// - otherwise not eligible.
pub fn decide(task: &TaskFacts<'_>, runner: &RunnerFacts<'_>, pool_teams: &[TeamId]) -> Eligibility {
    match task.assignee_id {
        Some(assignee) if assignee == runner.owner => Eligibility::Assigned,
        Some(_) => Eligibility::NotEligible(Reason::AssignedToSomeoneElse),
        None if task.personal_owner == Some(runner.owner) => Eligibility::Assigned,
        None if runner.policy == RunnerEligibility::AssignedThenPool
            && pool_teams.iter().any(|team| team == task.team_id) =>
        {
            Eligibility::Pool
        }
        None => Eligibility::NotEligible(Reason::Unassigned),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SHARED: &str = "team-shared";

    fn task(assignee: Option<&'static str>) -> TaskFacts<'static> {
        TaskFacts {
            assignee_id: assignee,
            team_id: SHARED,
            personal_owner: None,
        }
    }

    fn bobs(policy: RunnerEligibility) -> RunnerFacts<'static> {
        RunnerFacts {
            owner: "bob",
            policy,
        }
    }

    #[test]
    fn a_runner_never_runs_a_task_assigned_to_someone_else() {
        let pool = vec![SHARED.to_string()];
        for policy in [RunnerEligibility::Assigned, RunnerEligibility::AssignedThenPool] {
            assert_eq!(
                decide(&task(Some("alice")), &bobs(policy), &pool),
                Eligibility::NotEligible(Reason::AssignedToSomeoneElse),
                "{policy:?}"
            );
            assert_eq!(
                decide(&task(Some("bob")), &bobs(policy), &pool),
                Eligibility::Assigned,
                "{policy:?}"
            );
        }
    }

    #[test]
    fn an_unassigned_task_in_a_personal_team_is_the_owners() {
        let personal = TaskFacts {
            assignee_id: None,
            team_id: "team-personal",
            personal_owner: Some("bob"),
        };
        assert_eq!(
            decide(&personal, &bobs(RunnerEligibility::Assigned), &[]),
            Eligibility::Assigned
        );
        // Someone else's personal team is not this owner's.
        assert_eq!(
            decide(
                &TaskFacts {
                    personal_owner: Some("alice"),
                    ..personal
                },
                &bobs(RunnerEligibility::AssignedThenPool),
                &[]
            ),
            Eligibility::NotEligible(Reason::Unassigned)
        );
    }

    #[test]
    fn the_pool_is_only_the_teams_the_runner_opted_into() {
        let opted_in = vec![SHARED.to_string()];
        let elsewhere = vec!["team-other".to_string()];

        assert_eq!(
            decide(&task(None), &bobs(RunnerEligibility::AssignedThenPool), &opted_in),
            Eligibility::Pool
        );
        assert_eq!(
            decide(&task(None), &bobs(RunnerEligibility::AssignedThenPool), &elsewhere),
            Eligibility::NotEligible(Reason::Unassigned)
        );
        // A pool list without the policy is not a pool.
        assert_eq!(
            decide(&task(None), &bobs(RunnerEligibility::Assigned), &opted_in),
            Eligibility::NotEligible(Reason::Unassigned)
        );
    }
}
