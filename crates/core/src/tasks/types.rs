//! Input shapes the task service takes — never what it hands back.
//!
//! `db::models`'s own doc explains why a [`crate::db::Task`] only serializes:
//! what a caller *supplies* is a subset with its own optionality, and that
//! subset is these types.

use crate::db::{BoardColumn, RunState};

/// What [`crate::tasks::create_task`] takes.
#[derive(Debug, Clone)]
pub struct NewTask {
    pub repository_id: String,
    /// Validated non-blank by the service; the schema itself constrains only
    /// `NOT NULL` (ADR-0006 puts the business rule in code, not the CHECK).
    pub title: String,
    pub plan: Option<String>,
    pub extra_instructions: Option<String>,
    /// `None` takes ADR-0007's default: a freshly captured task starts in
    /// [`BoardColumn::NotReady`] — "captured, plan missing or incomplete".
    pub column: Option<BoardColumn>,
    /// Appended in the order given, at task-creation time only. Later links
    /// go through [`crate::tasks::add_task_link`].
    pub links: Vec<NewTaskLink>,
}

/// One `{label, url}` pair (ADR-0007) — for [`NewTask::links`] and
/// [`crate::tasks::add_task_link`] alike; a link supplied with the task and
/// one added afterwards are the same shape.
#[derive(Debug, Clone)]
pub struct NewTaskLink {
    pub label: String,
    pub url: String,
}

/// One field of a patch to a nullable column.
///
/// Plain `Option<T>` cannot say both "leave this alone" and "clear it" —
/// both collapse onto `None`. A caller that never mentions the field passes
/// [`Patch::Unset`]; [`Patch::Clear`] is a deliberate "set this column back
/// to NULL", and [`Patch::Set`] carries the replacement value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Patch<T> {
    #[default]
    Unset,
    Clear,
    Set(T),
}

impl<T> Patch<T> {
    /// Applies this field on top of `current`, leaving it untouched on
    /// [`Patch::Unset`]. What every field of [`TaskPatch`] and
    /// [`crate::tasks::update_task`] does with it.
    pub fn apply(self, current: Option<T>) -> Option<T> {
        match self {
            Patch::Unset => current,
            Patch::Clear => None,
            Patch::Set(value) => Some(value),
        }
    }
}

/// What [`crate::tasks::update_task`] takes. Only a field set to something
/// other than "unset" changes — patch semantics, never a whole-row
/// replacement.
///
/// Deliberately absent: `column` (only [`crate::tasks::move_task`] moves a
/// card, because a column change also recomputes `position`), `run_state`
/// (only [`crate::tasks::set_run_state`] writes it, because it validates the
/// transition), and links (their own add/update/remove/reorder surface).
#[derive(Debug, Clone, Default)]
pub struct TaskPatch {
    /// `None` leaves the task in the repository it is already filed under.
    /// A plain `Option` rather than a [`Patch`] for the same reason `title`
    /// is one: `repository_id` is `NOT NULL`, so there is nothing for "clear
    /// it" to mean.
    ///
    /// Naming a *different* repository is only legal while the task has no
    /// worktree and no runs (seam-contract D13) —
    /// [`crate::tasks::update_task`] refuses otherwise, naming what blocks
    /// it, and also moves the card to the bottom of its column in the
    /// destination, because `position` is scoped to `(repository, column)`
    /// (ADR-0007).
    pub repository_id: Option<String>,
    /// `None` leaves the title alone. Never [`Patch::Clear`]: `title` is
    /// `NOT NULL`, and non-blank is a rule this crate enforces rather than
    /// the schema, so there is nothing here to represent "clear it" with.
    pub title: Option<String>,
    pub plan: Patch<String>,
    pub extra_instructions: Patch<String>,
    pub model: Patch<String>,
    pub effort: Patch<String>,
}

/// What [`crate::tasks::list_tasks`] filters on. A field left `None` matches
/// everything; combining fields narrows the result, it never widens it.
#[derive(Debug, Clone, Default)]
pub struct TaskFilter {
    pub repository_id: Option<String>,
    pub column: Option<BoardColumn>,
    pub run_state: Option<RunState>,
}

/// What [`crate::tasks::update_task_link`] takes. `label` and `url` are both
/// `NOT NULL`, so — unlike [`TaskPatch`]'s nullable columns — plain `Option`
/// says everything needed: there is no "clear" to represent.
#[derive(Debug, Clone, Default)]
pub struct TaskLinkPatch {
    pub label: Option<String>,
    pub url: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn unset_leaves_the_current_value_alone() {
        assert_eq!(
            Patch::Unset.apply(Some("old".to_string())),
            Some("old".to_string())
        );
        assert_eq!(Patch::<String>::Unset.apply(None), None);
    }

    #[test]
    fn clear_always_produces_none() {
        assert_eq!(Patch::Clear.apply(Some("old".to_string())), None);
        assert_eq!(Patch::<String>::Clear.apply(None), None);
    }

    #[test]
    fn set_replaces_whatever_was_there() {
        assert_eq!(
            Patch::Set("new".to_string()).apply(Some("old".to_string())),
            Some("new".to_string())
        );
        assert_eq!(
            Patch::Set("new".to_string()).apply(None),
            Some("new".to_string())
        );
    }
}
