//! The task service layer: CRUD, board columns, ordering, run-state
//! transitions, links, and dependency edges (ADR-0007, ADR-0008; tasks 004 and
//! 010).
//!
//! Task 004's original note assigned every dependency semantic to task 011.
//! Seam-contract D16 moved half of it here: [`dependencies`] ships the write —
//! replace-the-whole-set, cycle detection, cross-repository rejection —
//! because `set_task_dependencies` is on ADR-0006's tool table and task 010
//! ships that tool. What is still task 011's: the `blocked` run state, the
//! scheduler predicate, `blocked_by_incomplete`, branch chaining and the UI.
//!
//! `position` is a fractional float and ordering *is* the priority mechanism —
//! there is no separate priority field. A dependency is satisfied when its run
//! succeeds, not when a human marks it done; that is deliberate and
//! load-bearing.
//!
//! Every function here takes [`ServiceContext`](crate::ServiceContext), not a
//! bare `&SqlitePool` — ADR-0018 amends task 004's original "no `AppHandle`,
//! no `tauri::State`" note to this shape once a mutation also has to publish
//! a [`ChangeEvent`](crate::ChangeEvent). The constraint itself is unchanged:
//! nothing here is a shell type, so the MCP server (ADR-0006) enforces the
//! same invariants as the UI by calling the same code.

pub mod dependencies;
pub mod links;
pub mod position;
pub mod run_state;
pub mod service;
pub mod strategy;
pub mod types;

pub use dependencies::set_task_dependencies;
pub use links::{
    add_task_link, get_task_link, remove_task_link, reorder_task_link, update_task_link,
};
pub use position::{position_between, rebalance_column, rebalanced_positions, Placement};
pub use run_state::{is_legal_run_state_transition, run_state_spelling, set_run_state};
pub use service::{
    create_task, delete_task, get_task, list_tasks, move_task, move_task_to_bottom, update_task,
    LastRunSummary, TaskDetail, TaskSummary,
};
pub use strategy::{
    accept_task_strategy, clear_task_strategy, needs_planning, set_task_strategy, StrategyPhase,
    StrategyPlan, StrategyPlanRun, StrategyPlanStatus, StrategyWorkflow,
};
pub use types::{NewTask, NewTaskLink, Patch, TaskFilter, TaskLinkPatch, TaskPatch};
