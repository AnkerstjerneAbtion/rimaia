//! The task service layer: CRUD, board columns, ordering, run-state
//! transitions, and links (ADR-0007, task 004). Dependency edges are stored
//! and read here — the delete guard needs them — but their semantics
//! (cycle rejection, blocking) are task 011's (ADR-0008).
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

pub mod links;
pub mod position;
pub mod run_state;
pub mod service;
pub mod types;

pub use links::{add_task_link, remove_task_link, reorder_task_link, update_task_link};
pub use position::{position_between, rebalance_column, rebalanced_positions, Placement};
pub use run_state::{is_legal_run_state_transition, set_run_state};
pub use service::{
    create_task, delete_task, get_task, list_tasks, move_task, update_task, LastRunSummary,
    TaskDetail, TaskSummary,
};
pub use types::{NewTask, NewTaskLink, Patch, TaskFilter, TaskLinkPatch, TaskPatch};
