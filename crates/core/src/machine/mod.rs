//! What one machine keeps for itself, behind a port (ADR-0028 point 2,
//! ADR-0027 point 6, seam-contract D31's 2026-10-10 amendment).
//!
//! The runner's settings, its schedules, its checkouts and its worktree
//! records belong to one machine, and from task 041 they live in `runner.db`.
//! That splits each fact in two across a crate boundary `rimaia-core` must
//! never cross: the **rules** (what a key means, what is valid, what is
//! announced) stay here, next to the code that owns each fact (D3), because
//! the local MCP handlers that call them are core code; the **queries** live
//! in `rimaia-runner` (D33 point 2). [`MachineStore`] is the line between
//! them, implemented by the runner on `runner.db` and injected by the host as
//! [`MachineContext`].
//!
//! It is the machine's counterpart of [`crate::board`]'s port, and shaped like
//! it: boxed futures, held as an `Arc<dyn …>`, built once per host, and bound
//! to its in-memory twin by a contract suite
//! (`testing::machine_contract`).

pub mod adoption;
pub mod context;
pub mod local;
pub mod port;
pub mod types;

pub use context::MachineContext;
pub use local::{checkout_of, consented_repositories, not_set_up, CheckoutView};
pub use port::{MachineFuture, MachineStore};
pub use types::{Checkout, CheckoutPatch, WorktreeRecord};
