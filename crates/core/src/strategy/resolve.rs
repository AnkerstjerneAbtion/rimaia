//! The precedence chain: task, then repository, then global (ADR-0016,
//! seam-contract D17.6).
//!
//! Pure. No [`ServiceContext`](crate::ServiceContext), no pool, no clock —
//! [`effective_strategy`] takes the three values it decides between and returns
//! the answer. That is deliberate rather than incidental: the same function
//! fills the badge on every card in a board read, decides the two flags a spawn
//! gets, and answers "why is this task on Opus?" in the detail panel, and a
//! version of it that did its own I/O could not be called from all three.
//!
//! The subtlety worth the module: `tasks.strategy_mode` is
//! `NOT NULL DEFAULT 'default'`, so it **cannot spell "inherit"**.
//! [`StrategyMode::Default`] on a task therefore has to mean *fall through* —
//! otherwise a repository default could never reach a card that nobody had
//! touched, and ADR-0016's "a repo of small tasks can default low without
//! touching each card" would be unimplementable.

use crate::db::{StrategyMode, Task};
use crate::strategy::settings::StrategyDefaults;

/// Who decided a value, so the panel can say so and the card can mute a badge
/// nobody chose.
///
/// [`ClaudeCode`](StrategyOrigin::ClaudeCode) is the fourth because "nothing is
/// configured" is an answer with consequences — no `--model` reaches argv and
/// the CLI picks — and folding it into `Global` would make the panel claim a
/// decision Rimaia never made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyOrigin {
    Task,
    Repository,
    Global,
    /// Unset everywhere: the flag is omitted and the CLI's own default applies.
    ClaudeCode,
}

/// What a run will actually spawn with, and where each half came from.
///
/// `model` and `effort` carry their own origin because they fall through
/// independently — a task that names a model and no effort takes the
/// repository's effort, and a single origin field would have to lie about one
/// of them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveStrategy {
    pub mode: StrategyMode,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub model_origin: StrategyOrigin,
    pub effort_origin: StrategyOrigin,
}

/// Resolves one task's strategy against its repository's default and the global
/// one.
///
/// Three rules, and the third is the one that surprises people:
///
/// 1. **Mode falls through.** A task in [`StrategyMode::Default`] has expressed
///    no preference, so the repository's mode applies, and then the global one.
///    A repository set to [`Planned`](StrategyMode::Planned) plans every
///    untouched card in it.
/// 2. **Model and effort fall through independently**, task before repository
///    before global, each ending at
///    [`ClaudeCode`](StrategyOrigin::ClaudeCode) when nobody has an opinion.
/// 3. **A task in *resolved* `Default` mode ignores its own model and effort.**
///    `Default` means "whatever is configured for me", and a card that also
///    carried a model would be spelling two answers at once. The consequence —
///    a model left on a card being silently dropped — is why
///    `tasks::update_task` flips the mode to [`Manual`](StrategyMode::Manual)
///    whenever a model or effort is set, and back when both are cleared
///    (D17.6). That rule lives in the service, not here, because this function
///    is also how a *stale* value gets ignored.
pub fn effective_strategy(
    task: &Task,
    repository_default: &StrategyDefaults,
    global_default: &StrategyDefaults,
) -> EffectiveStrategy {
    let mode = first_stated_mode([
        task.strategy_mode,
        repository_default.mode,
        global_default.mode,
    ]);

    // Rule 3: in resolved `Default` mode the task's own two fields are not part
    // of the chain at all, rather than being last in it.
    let task_model = (mode != StrategyMode::Default)
        .then_some(task.model.as_deref())
        .flatten();
    let task_effort = (mode != StrategyMode::Default)
        .then_some(task.effort.as_deref())
        .flatten();

    let (model, model_origin) = first_stated(
        task_model,
        repository_default.model.as_deref(),
        global_default.model.as_deref(),
    );
    let (effort, effort_origin) = first_stated(
        task_effort,
        repository_default.effort.as_deref(),
        global_default.effort.as_deref(),
    );

    EffectiveStrategy {
        mode,
        model,
        effort,
        model_origin,
        effort_origin,
    }
}

/// The first level that stated a mode, or [`StrategyMode::Default`] when none
/// did — which is itself a legal answer, and the one an untouched install gives.
fn first_stated_mode(levels: [StrategyMode; 3]) -> StrategyMode {
    levels
        .into_iter()
        .find(|mode| *mode != StrategyMode::Default)
        .unwrap_or(StrategyMode::Default)
}

/// The first level that stated a value, with the origin to explain it.
///
/// One function for both fields, so "independently" is a property of the code
/// and not of two call sites that happen to agree today.
fn first_stated(
    task: Option<&str>,
    repository: Option<&str>,
    global: Option<&str>,
) -> (Option<String>, StrategyOrigin) {
    for (value, origin) in [
        (task, StrategyOrigin::Task),
        (repository, StrategyOrigin::Repository),
        (global, StrategyOrigin::Global),
    ] {
        if let Some(value) = value {
            return (Some(value.to_string()), origin);
        }
    }

    (None, StrategyOrigin::ClaudeCode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{BoardColumn, MutationSource, RunState};
    use crate::testing::test_epoch;
    use pretty_assertions::assert_eq;

    #[test]
    fn a_task_that_names_a_model_outranks_the_repository_default() {
        let task = task(StrategyMode::Manual, Some("opus"), Some("max"));

        let effective = effective_strategy(
            &task,
            &defaults(StrategyMode::Default, Some("haiku"), Some("low")),
            &defaults(StrategyMode::Default, Some("sonnet"), Some("medium")),
        );

        assert_eq!(effective.model.as_deref(), Some("opus"));
        assert_eq!(effective.effort.as_deref(), Some("max"));
        assert_eq!(effective.model_origin, StrategyOrigin::Task);
        assert_eq!(effective.effort_origin, StrategyOrigin::Task);
        assert_eq!(effective.mode, StrategyMode::Manual);
    }

    #[test]
    fn a_task_in_default_mode_ignores_a_stale_model_and_takes_the_repository_default() {
        // The card someone set to Opus and then switched back to Default. The
        // column still holds "opus"; `default` means "whatever is configured
        // for me", and this is the read that makes that true (D17.6).
        let task = task(StrategyMode::Default, Some("opus"), Some("max"));

        let effective = effective_strategy(
            &task,
            &defaults(StrategyMode::Default, Some("haiku"), Some("low")),
            &defaults(StrategyMode::Default, None, None),
        );

        assert_eq!(effective.model.as_deref(), Some("haiku"));
        assert_eq!(effective.effort.as_deref(), Some("low"));
        assert_eq!(effective.model_origin, StrategyOrigin::Repository);
        assert_eq!(effective.effort_origin, StrategyOrigin::Repository);
    }

    #[test]
    fn an_absent_repository_default_falls_through_to_the_global_one() {
        let task = task(StrategyMode::Default, None, None);

        let effective = effective_strategy(
            &task,
            &StrategyDefaults::default(),
            &defaults(StrategyMode::Manual, Some("sonnet"), Some("medium")),
        );

        assert_eq!(effective.mode, StrategyMode::Manual);
        assert_eq!(effective.model.as_deref(), Some("sonnet"));
        assert_eq!(effective.effort.as_deref(), Some("medium"));
        assert_eq!(effective.model_origin, StrategyOrigin::Global);
        assert_eq!(effective.effort_origin, StrategyOrigin::Global);
    }

    #[test]
    fn model_and_effort_fall_through_independently_of_each_other() {
        // The whole reason `EffectiveStrategy` carries two origins: a task that
        // pinned a model has said nothing about effort, and the level that
        // answers each question is not the same level.
        let task = task(StrategyMode::Manual, Some("opus"), None);

        let effective = effective_strategy(
            &task,
            &defaults(StrategyMode::Default, None, Some("high")),
            &defaults(StrategyMode::Default, Some("sonnet"), Some("low")),
        );

        assert_eq!(effective.model.as_deref(), Some("opus"));
        assert_eq!(effective.model_origin, StrategyOrigin::Task);
        assert_eq!(effective.effort.as_deref(), Some("high"));
        assert_eq!(effective.effort_origin, StrategyOrigin::Repository);
    }

    #[test]
    fn a_repository_default_of_planned_makes_an_untouched_task_planned() {
        // ADR-0016's "a repo of small tasks can default low without touching
        // each card", in the direction that costs money: every card in this
        // repository now gets a planner run.
        let task = task(StrategyMode::Default, None, None);

        let effective = effective_strategy(
            &task,
            &defaults(StrategyMode::Planned, None, None),
            &defaults(StrategyMode::Manual, Some("sonnet"), None),
        );

        assert_eq!(
            effective.mode,
            StrategyMode::Planned,
            "the nearer level wins the mode, exactly as it wins the model"
        );
        assert_eq!(effective.model.as_deref(), Some("sonnet"));
        assert_eq!(effective.model_origin, StrategyOrigin::Global);
    }

    #[test]
    fn nothing_configured_anywhere_leaves_both_flags_off_so_the_cli_chooses() {
        let task = task(StrategyMode::Default, None, None);

        let effective = effective_strategy(
            &task,
            &StrategyDefaults::default(),
            &StrategyDefaults::default(),
        );

        assert_eq!(effective.mode, StrategyMode::Default);
        assert_eq!(effective.model, None);
        assert_eq!(effective.effort, None);
        assert_eq!(effective.model_origin, StrategyOrigin::ClaudeCode);
        assert_eq!(effective.effort_origin, StrategyOrigin::ClaudeCode);
    }

    #[test]
    fn a_planned_task_still_carries_the_model_its_planner_wrote() {
        // `planned` is not `default`: once a proposal has been applied to the
        // card, the columns it wrote are the ones that spawn.
        let task = task(StrategyMode::Planned, Some("sonnet"), Some("high"));

        let effective = effective_strategy(
            &task,
            &defaults(StrategyMode::Default, Some("haiku"), Some("low")),
            &StrategyDefaults::default(),
        );

        assert_eq!(effective.mode, StrategyMode::Planned);
        assert_eq!(effective.model.as_deref(), Some("sonnet"));
        assert_eq!(effective.effort.as_deref(), Some("high"));
        assert_eq!(effective.model_origin, StrategyOrigin::Task);
    }

    fn defaults(mode: StrategyMode, model: Option<&str>, effort: Option<&str>) -> StrategyDefaults {
        StrategyDefaults {
            mode,
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
        }
    }

    /// A `tasks` row with everything this function does not read left at its
    /// most boring value — the three fields under test are the arguments.
    fn task(mode: StrategyMode, model: Option<&str>, effort: Option<&str>) -> Task {
        Task {
            id: "3f2b1c00-0000-4000-8000-000000000001".to_string(),
            repository_id: "3f2b1c00-0000-4000-8000-000000000002".to_string(),
            title: "Wire the board to the store".to_string(),
            plan: None,
            extra_instructions: None,
            column: BoardColumn::Ready,
            position: 1.0,
            run_state: RunState::Idle,
            branch: None,
            worktree_path: None,
            strategy_mode: mode,
            model: model.map(str::to_string),
            effort: effort.map(str::to_string),
            strategy_plan: None,
            strategy_source: None,
            strategy_updated_at: None,
            created_at: test_epoch(),
            updated_at: test_epoch(),
            source: MutationSource::Ui,
        }
    }
}
